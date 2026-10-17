param(
    [Parameter(Mandatory = $true)][string]$Executable,
    [string]$ArgumentLine = '',
    [string]$WorkingDirectory = (Get-Location).Path,
    [Parameter(Mandatory = $true)][string]$OutputDirectory,
    [int]$Width = 160,
    [int]$Height = 48,
    [int]$DurationSeconds = 60,
    [string]$InputFile,
    [int]$InputDelayMilliseconds = 500
)

$ErrorActionPreference = 'Stop'
if ($Width -lt 20 -or $Height -lt 5) { throw 'ConPTY dimensions must be at least 20x5.' }
if ($DurationSeconds -lt 1) { throw 'DurationSeconds must be positive.' }
$exe = (Resolve-Path -LiteralPath $Executable).Path
$cwd = (Resolve-Path -LiteralPath $WorkingDirectory).Path
$inputPath = if ($InputFile) { (Resolve-Path -LiteralPath $InputFile).Path } else { $null }
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$out = Join-Path $OutputDirectory "conpty-$stamp"
New-Item -ItemType Directory -Path $out -Force | Out-Null

$nativeSource = @'
using System;
using System.ComponentModel;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
using System.Threading.Tasks;

public static class ConPtyCapture {
  const uint EXTENDED_STARTUPINFO_PRESENT=0x00080000, CREATE_UNICODE_ENVIRONMENT=0x00000400;
  const int PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE=0x00020016;
  [StructLayout(LayoutKind.Sequential)] struct COORD { public short X,Y; public COORD(short x, short y){X=x;Y=y;} }
  [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)] struct STARTUPINFO { public int cb; public string reserved,desktop,title; public int x,y,xSize,ySize,xCountChars,yCountChars,fillAttribute,flags; public short showWindow,reserved2; public IntPtr reserved2Ptr,stdInput,stdOutput,stdError; }
  [StructLayout(LayoutKind.Sequential)] struct STARTUPINFOEX { public STARTUPINFO StartupInfo; public IntPtr AttributeList; }
  [StructLayout(LayoutKind.Sequential)] struct PROCESS_INFORMATION { public IntPtr Process,Thread; public int ProcessId,ThreadId; }
  [DllImport("kernel32.dll", SetLastError=true)] static extern bool CreatePipe(out IntPtr readPipe,out IntPtr writePipe,IntPtr attributes,int size);
  [DllImport("kernel32.dll", SetLastError=true)] static extern bool CloseHandle(IntPtr handle);
  [DllImport("kernel32.dll", SetLastError=true)] static extern int CreatePseudoConsole(COORD size,IntPtr input,IntPtr output,uint flags,out IntPtr pseudoConsole);
  [DllImport("kernel32.dll")] static extern void ClosePseudoConsole(IntPtr pseudoConsole);
  [DllImport("kernel32.dll", SetLastError=true)] static extern bool InitializeProcThreadAttributeList(IntPtr list,int count,int flags,ref IntPtr size);
  [DllImport("kernel32.dll", SetLastError=true)] static extern bool UpdateProcThreadAttribute(IntPtr list,uint flags,IntPtr attribute,IntPtr value,IntPtr size,IntPtr previous,IntPtr returned);
  [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool CreateProcess(string app,StringBuilder command,IntPtr processAttributes,IntPtr threadAttributes,bool inherit,uint flags,IntPtr environment,string currentDirectory,ref STARTUPINFOEX startup,out PROCESS_INFORMATION info);
  [DllImport("kernel32.dll", SetLastError=true)] static extern bool ReadFile(IntPtr file,byte[] buffer,uint count,out uint read,IntPtr overlapped);
  [DllImport("kernel32.dll", SetLastError=true)] static extern bool WriteFile(IntPtr file,byte[] buffer,uint count,out uint written,IntPtr overlapped);
  [DllImport("kernel32.dll", SetLastError=true)] static extern uint WaitForSingleObject(IntPtr handle,uint milliseconds);
  [DllImport("kernel32.dll", SetLastError=true)] static extern bool TerminateProcess(IntPtr process,uint code);
  [DllImport("kernel32.dll")] static extern bool QueryPerformanceCounter(out long value);
  [DllImport("kernel32.dll")] static extern bool QueryPerformanceFrequency(out long value);
  static void Check(bool ok,string name){if(!ok)throw new Win32Exception(Marshal.GetLastWin32Error(),name);}
  static string Quote(string s){if(s.Length==0)return "\"\""; if(s.IndexOfAny(new[]{' ','\t','\n','\v','\"'})<0)return s; var b=new StringBuilder("\""); int slashes=0; foreach(char c in s){if(c=='\\'){slashes++;continue;} if(c=='\"'){b.Append('\\',slashes*2+1).Append(c);slashes=0;continue;} b.Append('\\',slashes).Append(c);slashes=0;} b.Append('\\',slashes*2).Append('\"');return b.ToString();}
  public static object Run(string exe,string args,string cwd,string output,int width,int height,int timeoutMs,string inputFile,int inputDelayMs){
    IntPtr inputRead=IntPtr.Zero,inputWrite=IntPtr.Zero,outputRead=IntPtr.Zero,outputWrite=IntPtr.Zero,hpc=IntPtr.Zero,attrs=IntPtr.Zero;
    PROCESS_INFORMATION pi=new PROCESS_INFORMATION(); bool timedOut=false; long frequency; QueryPerformanceFrequency(out frequency);
    var raw=Path.Combine(output,"ansi-output.bin");var samples=Path.Combine(output,"conpty-read-qpc.jsonl"); long total=0; int reads=0;
    try {
      Check(CreatePipe(out inputRead,out inputWrite,IntPtr.Zero,0),"CreatePipe(input)"); Check(CreatePipe(out outputRead,out outputWrite,IntPtr.Zero,0),"CreatePipe(output)");
      int hr=CreatePseudoConsole(new COORD((short)width,(short)height),inputRead,outputWrite,0,out hpc); if(hr<0)Marshal.ThrowExceptionForHR(hr);
      IntPtr attrBytes=IntPtr.Zero; InitializeProcThreadAttributeList(IntPtr.Zero,1,0,ref attrBytes); attrs=Marshal.AllocHGlobal(attrBytes);
      Check(InitializeProcThreadAttributeList(attrs,1,0,ref attrBytes),"InitializeProcThreadAttributeList");
      Check(UpdateProcThreadAttribute(attrs,0,new IntPtr(PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE),hpc,new IntPtr(IntPtr.Size),IntPtr.Zero,IntPtr.Zero),"UpdateProcThreadAttribute(PSEUDOCONSOLE)");
      var sx=new STARTUPINFOEX();sx.StartupInfo.cb=Marshal.SizeOf(typeof(STARTUPINFOEX));sx.AttributeList=attrs;
      // Match Windows Terminal's ConptyConnection: explicit null standard
      // handles prevent inheriting the collector's outer console handles.
      sx.StartupInfo.flags=0x00000100; // STARTF_USESTDHANDLES
      string command=Quote(exe)+(String.IsNullOrWhiteSpace(args)?"":" "+args);
      Check(CreateProcess(null,new StringBuilder(command),IntPtr.Zero,IntPtr.Zero,false,EXTENDED_STARTUPINFO_PRESENT,IntPtr.Zero,cwd,ref sx,out pi),"CreateProcess(ConPTY child)");
      CloseHandle(inputRead);inputRead=IntPtr.Zero;CloseHandle(outputWrite);outputWrite=IntPtr.Zero;
      using(var file=new FileStream(raw,FileMode.CreateNew,FileAccess.Write,FileShare.Read)) using(var log=new StreamWriter(new FileStream(samples,FileMode.CreateNew,FileAccess.Write,FileShare.Read),new UTF8Encoding(false))) {
        var reader=Task.Run(()=>{var buffer=new byte[65536];while(true){uint n;long before,after;QueryPerformanceCounter(out before);bool ok=ReadFile(outputRead,buffer,(uint)buffer.Length,out n,IntPtr.Zero);QueryPerformanceCounter(out after);if(!ok||n==0)break;file.Write(buffer,0,(int)n);file.Flush();log.WriteLine("{\"qpc_before\":"+before+",\"qpc_after\":"+after+",\"bytes\":"+n+"}");log.Flush();Interlocked.Add(ref total,n);Interlocked.Increment(ref reads);}});
        Task input=Task.Run(()=>{if(String.IsNullOrEmpty(inputFile))return;Thread.Sleep(inputDelayMs);byte[] data=File.ReadAllBytes(inputFile);uint written;Check(WriteFile(inputWrite,data,(uint)data.Length,out written,IntPtr.Zero),"WriteFile(input script)");if(written!=data.Length)throw new IOException("Short ConPTY input write.");});
        uint wait=WaitForSingleObject(pi.Process,(uint)timeoutMs);if(wait==0x102){timedOut=true;TerminateProcess(pi.Process,124);WaitForSingleObject(pi.Process,5000);}else if(wait==0xFFFFFFFF)throw new Win32Exception(Marshal.GetLastWin32Error(),"WaitForSingleObject");
        // Let the ConPTY host flush the child's final VT output before closing
        // the pseudoconsole. Microsoft’s EchoCon sample allows a short drain
        // interval after the child process exits for this reason.
        Thread.Sleep(500);
        ClosePseudoConsole(hpc);hpc=IntPtr.Zero;
        if(!reader.Wait(10000))throw new TimeoutException("ConPTY output reader did not reach EOF."); input.Wait(10000);
      }
      uint processExit;Check(GetExitCodeProcess(pi.Process,out processExit),"GetExitCodeProcess");
      return new {processId=pi.ProcessId,exitCode=timedOut?124:(int)processExit,timedOut,bytes=total,readChunks=reads,qpcFrequency=frequency,outputFile=raw,samplesFile=samples};
    } finally {if(hpc!=IntPtr.Zero)ClosePseudoConsole(hpc);if(attrs!=IntPtr.Zero){DeleteProcThreadAttributeList(attrs);Marshal.FreeHGlobal(attrs);}foreach(var h in new[]{inputRead,inputWrite,outputRead,outputWrite,pi.Thread,pi.Process})if(h!=IntPtr.Zero)CloseHandle(h);}
  }
  [DllImport("kernel32.dll", SetLastError=true)] static extern bool DeleteProcThreadAttributeList(IntPtr list);
  [DllImport("kernel32.dll", SetLastError=true)] static extern bool GetExitCodeProcess(IntPtr process,out uint code);
}
'@
Add-Type -TypeDefinition $nativeSource
$started = Get-Date
$capture = [ConPtyCapture]::Run($exe, $ArgumentLine, $cwd, $out, $Width, $Height, $DurationSeconds * 1000, $inputPath, $InputDelayMilliseconds)
$hashAlgorithm = [Security.Cryptography.SHA256]::Create()
$exeStream = [IO.File]::OpenRead($exe)
try {
    $exeHash = [BitConverter]::ToString($hashAlgorithm.ComputeHash($exeStream)).Replace('-', '')
} finally {
    $exeStream.Dispose()
    $hashAlgorithm.Dispose()
}
[pscustomobject]@{
    capturedAt = $started.ToString('o')
    finishedAt = (Get-Date).ToString('o')
    executable = $exe
    executableSha256 = $exeHash
    argumentLine = $ArgumentLine
    workingDirectory = $cwd
    width = $Width
    height = $Height
    requestedDurationSeconds = $DurationSeconds
    inputFile = $inputPath
    capture = $capture
    interpretation = 'QPC samples are ConPTY pipe-read arrival intervals. They do not measure application flush time, ANSI-to-Ratatui timing, or physical frame presentation.'
} | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $out 'capture.json') -Encoding utf8
Write-Output "ConPTY capture package: $out"
