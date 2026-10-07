# PowerShell 5.1 guest supervisor. All mutable state stays in the Defender-excluded
# AFT build directory; provisioning and the VM smoke fixture are read-only to us.
param(
    [ValidateSet('Supervisor', 'Worker')][string]$Mode = 'Supervisor',
    [ValidatePattern('^[0-9a-f]{32}$')][string]$RunId,
    [int]$CapSeconds = 3600,
    [string]$Holder
)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
. C:\build\provision\dev-shell.ps1
$Root = 'C:\build\aft'
$Repo = Join-Path $Root 'repo'
$Run = Join-Path $Root "runs\$RunId"
$LockPath = Join-Path $Root 'gate.lock'

function Invoke-Native([string]$Program, [string[]]$Arguments) {
    # Windows PowerShell wraps native stderr as ErrorRecords. Exit codes, not
    # cargo's ordinary stderr progress messages, decide whether a command failed.
    $ErrorActionPreference = 'Continue'
    & $Program @Arguments 2>&1 | ForEach-Object { Write-Output $_.ToString() }
    $code = $LASTEXITCODE
    if ($code -ne 0) { throw "$Program exited $code" }
}

function Stop-Worker($Process) {
    if ($null -ne $Process -and -not $Process.HasExited) {
        # Stop-Process alone leaves cargo, rustc and test subprocesses alive.
        $ErrorActionPreference = 'Continue'
        & taskkill.exe /PID $Process.Id /T /F 2>&1 | ForEach-Object { Write-Output $_.ToString() }
        $Process.WaitForExit()
    }
}

function Invoke-Worker {
    $plan = Get-Content (Join-Path $Run 'plan.json') -Raw | ConvertFrom-Json
    if ($plan.sha -notmatch '^[0-9a-f]{40}$') { throw 'Invalid commit in plan' }
    $bundle = Join-Path $Run 'commit.bundle'
    if (-not (Test-Path (Join-Path $Repo '.git'))) {
        # A snapshot rollback can remove the entire build root. A full bundle
        # bootstraps the same persistent checkout without network Git credentials.
        Invoke-Native git @('init', $Repo)
    }
    Set-Location $Repo
    Invoke-Native git @('config', 'core.autocrlf', 'false')
    Invoke-Native git @('config', 'core.hooksPath', 'NUL')
    Invoke-Native git @('bundle', 'verify', $bundle)
    Invoke-Native git @('-c', 'core.hooksPath=NUL', 'fetch', $bundle, 'HEAD')
    $existing = & git rev-parse HEAD 2>$null
    $dirty = & git status --porcelain --untracked-files=no
    if ($existing -ne $plan.sha -or $dirty) {
        Invoke-Native git @('-c', 'core.hooksPath=NUL', 'checkout', '--detach', '--force', $plan.sha)
    }
    # This is a dedicated, gate-owned checkout. Remove leftovers from fixture
    # crashes so untracked guest files cannot influence the next commit's build.
    Invoke-Native git @('clean', '-ffdx')
    $actual = (& git rev-parse HEAD).Trim()
    if ($LASTEXITCODE -ne 0 -or $actual -ne $plan.sha) { throw 'Guest checkout does not match the commit under test' }
    Write-Output "Guest exact commit: $actual"
    Invoke-Native cargo @('--version')
    Invoke-Native rustc @('-vV')
    if ((& rustc -vV | Out-String) -notmatch 'host: x86_64-pc-windows-msvc') { throw 'Expected native Windows x64 MSVC host' }

    # Keep only toolchain/dependency caches warm. Resolve these before replacing
    # HOME/USERPROFILE, otherwise rustup tries to find a toolchain in an empty home.
    if (-not $env:CARGO_HOME) { $env:CARGO_HOME = Join-Path $env:USERPROFILE '.cargo' }
    if (-not $env:RUSTUP_HOME) { $env:RUSTUP_HOME = Join-Path $env:USERPROFILE '.rustup' }
    $env:CARGO_TARGET_DIR = Join-Path $Root 'target'
    $env:CARGO_BUILD_JOBS = '4'
    Get-ChildItem Env: | Where-Object {
        $_.Name -match '^(AFT_|XDG_|GIT_CONFIG_|GIT_DIR$|GIT_WORK_TREE$)'
    } | ForEach-Object { Remove-Item "Env:$($_.Name)" }
    $homeRoot = Join-Path $Run 'homes'
    $homes = @{
        HOME = 'home'; USERPROFILE = 'home'; APPDATA = 'appdata'; LOCALAPPDATA = 'localappdata';
        XDG_DATA_HOME = 'data'; XDG_CONFIG_HOME = 'config'; XDG_CACHE_HOME = 'cache'; XDG_STATE_HOME = 'state';
        TEMP = 'temp'; TMP = 'temp'
    }
    foreach ($name in $homes.Keys) {
        $path = Join-Path $homeRoot $homes[$name]
        New-Item -ItemType Directory -Force $path | Out-Null
        Set-Item "Env:$name" $path
    }
    $env:HOMEDRIVE = 'C:'
    $env:HOMEPATH = $env:USERPROFILE.Substring(2)
    $env:AFT_RUST_TEST_GATE = '1'
    $env:AFT_GATE_HERMETIC_HOME_ROOT = $homeRoot
    $env:RUST_BACKTRACE = '1'
    $env:CARGO_TERM_COLOR = 'never'
    # Fixture commits must not consult builder's real Git identity or hooks.
    $env:GIT_CONFIG_NOSYSTEM = '1'
    $env:GIT_CONFIG_GLOBAL = Join-Path $env:HOME '.gitconfig'
    @('[user]', '  name = AFT Windows Gate', '  email = windows-gate@example.invalid',
      '[core]', '  hooksPath = NUL', '  autocrlf = false') | Set-Content $env:GIT_CONFIG_GLOBAL
    $failed = $false
    # Verify the production storage/config resolvers before any selected test can
    # spawn an AFT process. Environment assignment alone is not isolation proof.
    $isolationCheck = @{ kind = 'lib'; target = ''; filter = 'gate_hermeticity_tests::gate_resolves_every_user_config_and_state_path_under_the_gate_homes' }
    foreach ($job in (@($isolationCheck) + @($plan.jobs))) {
        if ($job.kind -notin @('lib', 'bin', 'test')) { throw 'Invalid test kind' }
        $cargoArgs = @('test', '--locked', '-j', '4', '-p', 'agent-file-tools', "--$($job.kind)")
        if ($job.target) { $cargoArgs += $job.target }
        if ($job.filter) { $cargoArgs += $job.filter }
        $cargoArgs += @('--', '--test-threads', '4')
        Write-Output "`n==> cargo $($cargoArgs -join ' ')"
        $output = New-Object 'System.Collections.Generic.List[string]'
        $ErrorActionPreference = 'Continue'
        & cargo @cargoArgs 2>&1 | ForEach-Object {
            $line = $_.ToString()
            $output.Add($line)
            Write-Output $line
        }
        $code = $LASTEXITCODE
        $ErrorActionPreference = 'Stop'
        if ($code -ne 0) {
            $failed = $true
            if ($job -eq $isolationCheck) { throw 'Storage isolation preflight failed; refusing to run selected tests' }
        }
        if (-not ($output | Where-Object { $_ -match 'test result: .* [1-9][0-9]* passed;' -or $_ -match 'test result: .* [1-9][0-9]* failed;' })) {
            Write-Output "GATE ERROR: Zero tests executed for $($cargoArgs -join ' '); an empty slice is not a pass."
            $failed = $true
            if ($job -eq $isolationCheck) { throw 'Storage isolation preflight did not execute; refusing to run selected tests' }
        }
    }
    if ($failed) { exit 1 }
    exit 0
}

if ($Mode -eq 'Worker') {
    try { Invoke-Worker } catch { Write-Output "GATE ERROR: $_"; exit 1 }
}

$lock = $null
$workerJob = $null
$worker = $null
$outReader = $null
$errReader = $null
$code = 1
$started = [DateTime]::UtcNow
try {
    New-Item -ItemType Directory -Force $Root | Out-Null
    try {
        $lock = [IO.File]::Open($LockPath, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read)
    } catch [IO.IOException] {
        $meta = $null
        try { $meta = Get-Content $LockPath -Raw | ConvertFrom-Json } catch { }
        $age = ([DateTime]::UtcNow - (Get-Item $LockPath).LastWriteTimeUtc).TotalSeconds
        $limit = $CapSeconds
        $owner = 'unknown (lock metadata incomplete)'
        if ($meta) {
            $age = ([DateTime]::UtcNow - [DateTime]::Parse($meta.started).ToUniversalTime()).TotalSeconds
            $limit = $meta.cap_seconds
            $owner = $meta.holder
        }
        if ($age -lt $limit) { throw ("Windows gate busy: holder {0}, age {1:N0}s, cap {2}s" -f $owner, $age, $limit) }
        # The held FileStream forbids deletion even after expiry. Never overlap a
        # live supervisor while it is killing a timed-out process tree.
        try { Remove-Item $LockPath -Force } catch {
            throw ("Windows gate busy: expired holder {0}, age {1:N0}s; supervisor still stopping. Retry shortly." -f $owner, $age)
        }
        $lock = [IO.File]::Open($LockPath, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read)
        Write-Output ("Reclaimed stale lock: holder {0}, age {1:N0}s" -f $owner, $age)
    }
    $meta = @{ holder = $Holder; started = $started.ToString('o'); cap_seconds = $CapSeconds; pid = $PID; run_id = $RunId }
    $bytes = [Text.Encoding]::UTF8.GetBytes(($meta | ConvertTo-Json -Compress))
    $lock.Write($bytes, 0, $bytes.Length)
    $lock.Flush()
    # Under the lock, expired upload directories are safe to remove. Keep recent
    # contenders intact until they get their explicit busy refusal.
    Get-ChildItem (Join-Path $Root 'runs') -Directory | Where-Object {
        $_.Name -ne $RunId -and ($started - $_.LastWriteTimeUtc).TotalSeconds -gt $CapSeconds
    } | Remove-Item -Recurse -Force
    $prerequisite = 'none'
    if (Test-Path (Join-Path $Repo '.git')) {
        $candidate = & git -C $Repo rev-parse HEAD 2>$null
        if ($LASTEXITCODE -eq 0) { $prerequisite = $candidate.Trim() }
    }
    Write-Output "AFT_WINDOWS_GATE_READY $prerequisite"
    $deadline = $started.AddSeconds($CapSeconds)
    while (-not (Test-Path (Join-Path $Run 'plan.json'))) {
        if (Test-Path (Join-Path $Run 'cancel')) { throw 'Gate cancelled during transfer' }
        if ([DateTime]::UtcNow -ge $deadline) { throw "TIMEOUT: Windows gate exceeded ${CapSeconds}s during transfer" }
        Start-Sleep -Milliseconds 200
    }
    $stdout = Join-Path $Run 'stdout.log'
    $stderr = Join-Path $Run 'stderr.log'
    # A kill-on-close Job Object also covers SSH disconnects or supervisor crashes:
    # the Windows kernel kills all descendants when the supervisor loses its handle.
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;
public static class AftWindowsGateJob {
    [StructLayout(LayoutKind.Sequential)] struct BasicLimits {
        public long ProcessTime, JobTime;
        public uint Flags;
        public UIntPtr MinWorkingSet, MaxWorkingSet;
        public uint ActiveProcesses;
        public UIntPtr Affinity;
        public uint Priority, Scheduling;
    }
    [StructLayout(LayoutKind.Sequential)] struct IoCounters {
        public ulong ReadOps, WriteOps, OtherOps, ReadBytes, WriteBytes, OtherBytes;
    }
    [StructLayout(LayoutKind.Sequential)] struct ExtendedLimits {
        public BasicLimits Basic;
        public IoCounters Io;
        public UIntPtr ProcessMemory, JobMemory, PeakProcessMemory, PeakJobMemory;
    }
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern SafeFileHandle CreateJobObject(IntPtr attributes, string name);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool SetInformationJobObject(SafeFileHandle job, int infoClass, ref ExtendedLimits info, uint length);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool AssignProcessToJobObject(SafeFileHandle job, IntPtr process);
    public static SafeFileHandle Create() {
        var job = CreateJobObject(IntPtr.Zero, null);
        if (job.IsInvalid) throw new Win32Exception(Marshal.GetLastWin32Error());
        var limits = new ExtendedLimits();
        limits.Basic.Flags = 0x2000; // JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        if (!SetInformationJobObject(job, 9, ref limits, (uint)Marshal.SizeOf(limits))) {
            var error = Marshal.GetLastWin32Error();
            job.Dispose();
            throw new Win32Exception(error);
        }
        return job;
    }
    public static void Attach(SafeFileHandle job, IntPtr process) {
        if (!AssignProcessToJobObject(job, process)) throw new Win32Exception(Marshal.GetLastWin32Error());
    }
}
'@
    $workerJob = [AftWindowsGateJob]::Create()
    $worker = Start-Process powershell.exe -ArgumentList @('-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass',
        '-File', (Join-Path $Run 'gate.ps1'), '-Mode', 'Worker', '-RunId', $RunId) -PassThru -NoNewWindow `
        -RedirectStandardOutput $stdout -RedirectStandardError $stderr
    # Cache the handle before a short-lived worker exits. Without it, Windows
    # PowerShell's Start-Process object can report a null ExitCode after exit.
    $null = $worker.Handle
    [AftWindowsGateJob]::Attach($workerJob, $worker.Handle)
    $outReader = New-Object IO.StreamReader([IO.File]::Open($stdout, 'Open', 'Read', 'ReadWrite'))
    $errReader = New-Object IO.StreamReader([IO.File]::Open($stderr, 'Open', 'Read', 'ReadWrite'))
    while ($true) {
        foreach ($reader in @($outReader, $errReader)) {
            while ($null -ne ($line = $reader.ReadLine())) { Write-Output $line }
        }
        if ($worker.HasExited) { break }
        if (Test-Path (Join-Path $Run 'cancel')) { throw 'Gate cancelled; stopping the Windows process tree' }
        if ([DateTime]::UtcNow -ge $deadline) { throw "TIMEOUT: Windows gate exceeded ${CapSeconds}s; stopping the Windows process tree" }
        Start-Sleep -Milliseconds 100
    }
    $worker.WaitForExit()
    # Drain bytes written between the last ReadLine and process exit.
    foreach ($reader in @($outReader, $errReader)) {
        while ($null -ne ($line = $reader.ReadLine())) { Write-Output $line }
    }
    if ($null -eq $worker.ExitCode) { throw 'Worker exit status unavailable; refusing a false pass' }
    $code = $worker.ExitCode
} catch {
    Write-Output "GATE ERROR: $_"
} finally {
    Stop-Worker $worker
    if ($workerJob) { $workerJob.Dispose() }
    if ($outReader) { $outReader.Dispose() }
    if ($errReader) { $errReader.Dispose() }
    if ($lock) {
        $lock.Dispose()
        Remove-Item $LockPath -Force
    }
    # Leave the current directory outside the run so PowerShell can delete it.
    Set-Location $Root
    if (Test-Path $Run) { Remove-Item $Run -Recurse -Force }
}
if ($code -eq 0) { Write-Output 'GATE PASSED' } else { Write-Output 'GATE FAILED' }
exit $code
