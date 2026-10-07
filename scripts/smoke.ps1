param([switch]$SkipBuild)
$ErrorActionPreference = 'Stop'
$projectPath = Split-Path -Parent $PSScriptRoot
Push-Location -LiteralPath $projectPath
$startedProcesses = [System.Collections.Generic.List[System.Diagnostics.Process]]::new()
$logPath = Join-Path $projectPath ('target/smoke-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $logPath | Out-Null
$serverExe = Join-Path $projectPath 'target/release/server.exe'
$makerExe = Join-Path $projectPath 'target/release/rfq-maker.exe'
$cliExe = Join-Path $projectPath 'target/release/rfq-cli.exe'
$script:cliCount = 0

function Start-TestProcess([string]$Executable, [string[]]$Arguments, [string]$Name) {
    $process = Start-Process -FilePath $Executable -ArgumentList $Arguments -WorkingDirectory $projectPath -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $logPath "$Name.out.log") -RedirectStandardError (Join-Path $logPath "$Name.err.log")
    # Keep the process handle so Windows PowerShell retains its exit code.
    $null = $process.Handle
    $startedProcesses.Add($process)
    return $process
}
function Invoke-TestCli([string[]]$CliArguments, [string]$Expected) {
    $script:cliCount++
    $name = 'cli-' + $script:cliCount
    $process = Start-TestProcess $cliExe (@('--server', $script:serverUrl) + $CliArguments) $name
    if (-not $process.WaitForExit(15000)) { throw "CLI timed out: $name" }
    $process.Refresh()
    $output = Get-Content -LiteralPath (Join-Path $logPath "$name.out.log") -Raw
    if ($process.ExitCode -ne 0) {
        $errorOutput = Get-Content -LiteralPath (Join-Path $logPath "$name.err.log") -Raw
        throw "CLI failed: $errorOutput"
    }
    if ($output -notmatch [regex]::Escape($Expected)) { throw "Expected '$Expected' in CLI output: $output" }
    Write-Output $output.Trim()
}
function Get-Totals($Balances) {
    $totals = @{}
    $totals['USD'] = [decimal]0
    foreach ($trader in $Balances.traders) {
        $totals['USD'] += [decimal]$trader.usd.total
        foreach ($property in $trader.assets.PSObject.Properties) {
            if (-not $totals.ContainsKey($property.Name)) { $totals[$property.Name] = [decimal]0 }
            $totals[$property.Name] += [decimal]$property.Value.total
        }
    }
    return $totals
}
try {
    if (-not $SkipBuild) {
        cargo build --release -p server -p cli -p maker
        if ($LASTEXITCODE -ne 0) { throw 'Release build failed.' }
    }
    # Pick an unused local port; leave any server the user already runs alone.
    $portProbe = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
    $portProbe.Start()
    $port = $portProbe.LocalEndpoint.Port
    $portProbe.Stop()
    $script:serverUrl = "http://127.0.0.1:$port"
    $server = Start-TestProcess $serverExe @('--seed', 'seed.toml', '--port', "$port", '--accept-window-secs', '3') 'server'
    $ready = $false
    for ($attempt = 0; $attempt -lt 100; $attempt++) {
        if ($server.HasExited) { throw 'Server exited during startup.' }
        try { $null = Invoke-RestMethod "$script:serverUrl/health"; $ready = $true; break } catch { Start-Sleep -Milliseconds 50 }
    }
    if (-not $ready) { throw 'Server did not become ready.' }
    $before = Get-Totals (Invoke-RestMethod "$script:serverUrl/v1/balances")
    Invoke-TestCli @('assets') 'SOL'
    Invoke-TestCli @('--key', 'keys/taker.json', 'submit', '--asset', 'SOL', '--size', '2', '--direction', 'buy', '--ttl', '1', '--yes') 'RFQ expired'
    foreach ($strategy in @('tight', 'lazy', 'skew', 'sniper')) {
        $maker = Start-TestProcess $makerExe @('--server', $script:serverUrl, '--key', "keys/maker-$strategy.json", '--strategy', $strategy, '--offline', '--rng-seed', '2026') "maker-$strategy"
        $ready = $false
        for ($attempt = 0; $attempt -lt 100; $attempt++) {
            if ($maker.HasExited) { throw "Maker $strategy exited during startup." }
            $out = Get-Content -LiteralPath (Join-Path $logPath "maker-$strategy.out.log") -Raw
            if ($out -match 'subscribed to /ws/makers') { $ready = $true; break }
            Start-Sleep -Milliseconds 50
        }
        if (-not $ready) { throw "Maker $strategy did not subscribe." }
    }
    foreach ($side in @('buy', 'sell')) {
        Invoke-TestCli @('--key', 'keys/taker.json', 'submit', '--asset', 'SOL', '--size', '2', '--direction', $side, '--ttl', '1', '--yes') 'RFQ settled'
        Invoke-TestCli @('--key', 'keys/taker.json', 'submit', '--asset', 'DOGE', '--size', '5000', '--ttl', '1', '--accept', $side) 'RFQ settled'
    }
    Invoke-TestCli @('balances') 'taker'
    $balances = Invoke-RestMethod "$script:serverUrl/v1/balances"
    $after = Get-Totals $balances
    foreach ($currency in $before.Keys) { if ($before[$currency] -ne $after[$currency]) { throw "$currency was not conserved." } }
    foreach ($trader in $balances.traders) {
        if ($trader.usd.reserved -ne 0) { throw 'USD reservation leaked.' }
        foreach ($property in $trader.assets.PSObject.Properties) { if ($property.Value.reserved -ne 0) { throw 'Asset reservation leaked.' } }
    }
    Write-Output "PASS: stock CLI and all four offline makers; directed and two-way buys/sells; empty RFQ; conservation and reservation cleanup. Logs: $logPath"
} finally {
    foreach ($process in $startedProcesses) {
        if (-not $process.HasExited) { Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue }
        $process.Dispose()
    }
    Pop-Location
}
