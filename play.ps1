[CmdletBinding()]
param(
    [Parameter(Position = 0)]
    [ValidateSet('help', 'build', 'server', 'makers', 'maker', 'buy', 'sell', 'quote', 'balances', 'assets', 'keygen')]
    [string]$Action = 'help',

    [Parameter(Position = 1)]
    [Alias('Amount', 'Who', 'Out', 'Strategy')]
    [string]$Value,

    [Parameter(Position = 2)]
    [string]$Asset = 'SOL',
    [Alias('Ttl')]
    [ValidateRange(1, 29)]
    [int]$Seconds = 5,
    [switch]$Yes,
    [ValidateSet('buy', 'sell')]
    [string]$Accept,
    [string]$Key,
    [string]$Url,
    [ValidateRange(1, 65535)]
    [int]$Port = 4402,
    [string]$Seed = 'seed.toml',
    [uint64]$AcceptSeconds = 30,
    [string]$Prices = 'prices.toml',
    [switch]$Live,
    [Nullable[uint64]]$RngSeed,
    [switch]$All,
    [switch]$DryRun
)

$ErrorActionPreference = 'Stop'
if (-not $Url) { $Url = "http://127.0.0.1:$Port" }
if ($Yes -and $Accept) { throw 'Use -Yes or -Accept, not both.' }
if ($Action -eq 'quote' -and $Yes) { throw 'Use -Accept buy or -Accept sell for a two-way quote.' }
if ($Action -in @('buy', 'sell') -and $Accept -and $Accept -ne $Action) {
    throw "A '$Action' request cannot accept the '$Accept' side. Use quote for both prices."
}
if ($All -and $Value) { throw 'Choose a trader or -All.' }
if ($PSBoundParameters.ContainsKey('Url') -and $PSBoundParameters.ContainsKey('Port')) {
    throw 'Use -Url or -Port, not both.'
}

$connectionOptions = @('Url', 'Port')
$allowedOptions = switch ($Action) {
    'help' { @() }
    'build' { @() }
    'server' { @('Seed', 'Port', 'AcceptSeconds') }
    'makers' { @('Prices', 'Live', 'RngSeed') + $connectionOptions }
    'maker' { @('Value', 'Key', 'Prices', 'Live', 'RngSeed') + $connectionOptions }
    { $_ -in @('buy', 'sell', 'quote') } { @('Value', 'Asset', 'Seconds', 'Yes', 'Accept', 'Key') + $connectionOptions }
    'balances' { @('Value', 'All') + $connectionOptions }
    'assets' { $connectionOptions }
    'keygen' { @('Value') }
}
foreach ($option in $PSBoundParameters.Keys) {
    if ($option -in @('Value', 'Asset', 'Seconds', 'Yes', 'Accept', 'Key', 'Url', 'Port', 'Seed', 'AcceptSeconds', 'Prices', 'Live', 'RngSeed', 'All') -and $option -notin $allowedOptions) {
        throw "-$option does not apply to '$Action'. Run .\play.ps1 help."
    }
}

function Invoke-PlayCommand {
    param([string]$Program, [string[]]$Arguments)
    if ($DryRun) {
        [pscustomobject]@{ Program = $Program; Arguments = $Arguments }
        return
    }
    if ($Program -ne 'cargo' -and -not (Test-Path -LiteralPath $Program)) {
        throw 'Run .\play.ps1 build first.'
    }
    & $Program @Arguments
    if ($LASTEXITCODE -ne 0) { throw 'Command failed; see the message above.' }
}

function Get-MakerArguments {
    param([string]$Strategy)
    $makerKey = if ($Key) { $Key } else { "keys/maker-$Strategy.json" }
    $arguments = @('--server', $Url, '--key', $makerKey, '--strategy', $Strategy, '--prices', $Prices)
    if (-not $Live) { $arguments += '--offline' }
    if ($null -ne $RngSeed) { $arguments += @('--rng-seed', "$RngSeed") }
    return $arguments
}

function ConvertTo-ProcessArgument {
    param([string]$Text)
    '"' + ($Text -replace '(\\*)"', '$1$1\"' -replace '(\\+)$', '$1$1') + '"'
}
Push-Location -LiteralPath $PSScriptRoot
try {
    if ($Action -eq 'help') {
        @'
Terminal 1:  .\play.ps1 server       Build and start the server
Terminal 2:  .\play.ps1 makers       Start all four competing bots
Terminal 3:  .\play.ps1 buy 2 SOL    Ask to buy 2 SOL

.\play.ps1 sell 2 SOL               Ask to sell 2 SOL
.\play.ps1 quote 2 SOL              Ask for both prices
.\play.ps1 balances                 Show your balances
.\play.ps1 assets                   List available coins
.\play.ps1 balances maker-tight     Any trader name or public key
.\play.ps1 balances -All            Everyone's balances
.\play.ps1 keygen keys/new.json     Generate a keypair
.\play.ps1 build                    Build without starting anything

.\play.ps1 buy 2 SOL -Yes           Automatically accept the buy quote
.\play.ps1 sell 2 SOL -Yes          Automatically accept the sell quote
.\play.ps1 quote 2 SOL -Accept buy  Get both prices, then automatically buy
.\play.ps1 quote 2 SOL -Accept sell Get both prices, then automatically sell
.\play.ps1 buy 0.5 SOL -Seconds 10  Give makers 10 seconds to quote (1-29)
.\play.ps1 buy 2 SOL -Key keys/other.json

.\play.ps1 maker tight             One bot: tight, lazy, skew, or sniper
.\play.ps1 maker lazy -Key keys/other.json
.\play.ps1 makers -Live             Live prices (default: offline)
.\play.ps1 makers -Prices prices.toml -RngSeed 2026
Single makers also support -Live, -Prices, and -RngSeed.

.\play.ps1 server -Seed seed.toml -Port 4403 -AcceptSeconds 15
.\play.ps1 makers -Port 4403
.\play.ps1 buy 2 SOL -Url http://127.0.0.1:4403
-Url or -Port works with trades, views, and makers.
.\play.ps1 buy 2 SOL -Yes -DryRun    Show arguments without executing

At the quote: b = buy, s = sell, d = decline, then Enter.
Stop makers and server with Ctrl+C in their terminals.
New keys need a trader entry and balances in the server's seed file.
'@
        return
    }

    if ($Action -in @('server', 'build')) {
        Invoke-PlayCommand 'cargo' @('build', '--release', '-p', 'server', '-p', 'cli', '-p', 'maker')
        if ($Action -eq 'server') {
            Invoke-PlayCommand '.\target\release\server.exe' @('--seed', $Seed, '--port', "$Port", '--accept-window-secs', "$AcceptSeconds")
        }
        return
    }

    if ($Action -eq 'maker') {
        $strategy = if ($Value) { $Value.ToLowerInvariant() } else { 'tight' }
        if ($strategy -notin @('tight', 'lazy', 'skew', 'sniper')) { throw "Unknown maker strategy: $strategy" }
        Invoke-PlayCommand '.\target\release\rfq-maker.exe' (Get-MakerArguments $strategy)
        return
    }

    if ($Action -eq 'makers') {
        if ($DryRun) {
            foreach ($strategy in @('tight', 'lazy', 'skew', 'sniper')) {
                Invoke-PlayCommand '.\target\release\rfq-maker.exe' (Get-MakerArguments $strategy)
            }
            return
        }
        Invoke-RestMethod "$($Url.TrimEnd('/'))/health" | Out-Null
        $logDirectory = Join-Path $PSScriptRoot ('target/play-' + [guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $logDirectory | Out-Null
        $bots = @()
        try {
            foreach ($strategy in @('tight', 'lazy', 'skew', 'sniper')) {
                $arguments = ((Get-MakerArguments $strategy) | ForEach-Object { ConvertTo-ProcessArgument $_ }) -join ' '
                $bots += Start-Process -FilePath '.\target\release\rfq-maker.exe' `
                    -ArgumentList $arguments `
                    -WorkingDirectory $PSScriptRoot -WindowStyle Hidden -PassThru `
                    -RedirectStandardOutput (Join-Path $logDirectory "$strategy.log") `
                    -RedirectStandardError (Join-Path $logDirectory "$strategy.err")
            }
            Write-Host 'Four makers are running. Trade in another terminal. Ctrl+C stops them all.'
            Write-Host "Bot logs: $logDirectory"
            while ($true) {
                foreach ($bot in $bots) {
                    if ($bot.HasExited) { throw "A maker stopped. See $logDirectory" }
                }
                Start-Sleep -Milliseconds 500
            }
        } finally {
            foreach ($bot in $bots) {
                if (-not $bot.HasExited) { $bot.Kill(); $bot.WaitForExit() }
                $bot.Dispose()
            }
        }
    } elseif ($Action -eq 'balances') {
        $arguments = @('--server', $Url, 'balances')
        if (-not $All -and $Value -ne 'all') { $arguments += $(if ($Value) { $Value } else { 'taker' }) }
        Invoke-PlayCommand '.\target\release\rfq-cli.exe' $arguments
    } elseif ($Action -eq 'assets') {
        Invoke-PlayCommand '.\target\release\rfq-cli.exe' @('--server', $Url, 'assets')
    } elseif ($Action -eq 'keygen') {
        if (-not $Value) { throw 'Choose an output file: .\play.ps1 keygen keys/new.json' }
        Invoke-PlayCommand '.\target\release\rfq-cli.exe' @('keygen', '--out', $Value)
    } else {
        $size = if ($Value) { $Value } else { '2' }
        $takerKey = if ($Key) { $Key } else { 'keys/taker.json' }
        $tradeArguments = @('--server', $Url, '--key', $takerKey, 'submit', '--asset', $Asset.ToUpperInvariant(), '--size', $size, '--ttl', "$Seconds")
        if ($Action -ne 'quote') { $tradeArguments += @('--direction', $Action) }
        if ($Yes) { $tradeArguments += '--yes' }
        if ($Accept) { $tradeArguments += @('--accept', $Accept) }
        Invoke-PlayCommand '.\target\release\rfq-cli.exe' $tradeArguments
    }
} finally {
    Pop-Location
}
