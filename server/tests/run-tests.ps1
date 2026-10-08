param(
    [ValidateRange(1, 20)]
    [int]$Requirement
)

$ErrorActionPreference = 'Stop'
$projectPath = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$groups = Get-Content (Join-Path $PSScriptRoot 'requirements.json') -Raw | ConvertFrom-Json

function Get-RegisteredTests {
    $listing = @(cargo test -p server -- --list)
    if ($LASTEXITCODE -ne 0) {
        throw 'Could not discover tests.'
    }
    $tests = @($listing | Where-Object { $_ -match ': test$' })
    if ($tests.Count -eq 0) {
        throw 'No tests were discovered.'
    }
    return $tests
}

# Validate coverage and registration before running any selected requirement.
function Assert-RequirementLayout {
    param($groups, $tests)
    $covered = @($groups | ForEach-Object { $_.requirements } | Sort-Object)
    if (($covered -join ',') -ne ((1..20) -join ',')) {
        throw 'Every requirement R1-R20 must appear exactly once in requirements.json.'
    }
    foreach ($group in $groups) {
        if (-not (Test-Path (Join-Path $PSScriptRoot "requirements/$($group.suite)"))) {
            throw "Missing folder: $($group.suite)"
        }
        if ($group.runner -eq 'cargo' -and @($tests | Where-Object { $_.Contains($group.filter) }).Count -eq 0) {
            throw "No runnable tests for $($group.suite)"
        }
    }
    foreach ($group in $groups) {
        $folder = Join-Path $PSScriptRoot "requirements/$($group.suite)"
        $rustFiles = @(Get-ChildItem $folder -Recurse -Filter '*.rs')
        if ($rustFiles.Count -ne 1 -or $rustFiles[0].Name -ne 'scenarios.rs') {
            throw "Each requirement folder must have exactly one scenarios.rs: $folder"
        }
        $source = Get-Content $rustFiles[0].FullName -Raw
        if ($source -match '\b(scenario!|macro_rules!|unwrap\(|reqwest::|(?:SubmitRfq|PlaceOrder|Accept|CancelOrder)Payload\s*\{)') {
            throw "Use ordinary test functions; move request plumbing into helpers: $folder/scenarios.rs"
        }
    }
    $declaredTests = 0
    foreach ($file in Get-ChildItem (Join-Path $PSScriptRoot 'requirements') -Recurse -Filter '*.rs') {
        $source = Get-Content $file.FullName -Raw
        $declarations = [regex]::Matches($source, '(?m)^\s*#\[(?:tokio::)?test[^\r\n]*\]\r?\n\s*(?:async )?fn ([a-zA-Z0-9_]+)')
        foreach ($declaration in $declarations) {
            $name = $declaration.Groups[1].Value
            if (@($tests | Where-Object { $_.EndsWith("::${name}: test") }).Count -ne 1) {
                throw "Test is missing or registered more than once: $name in $($file.FullName)"
            }
            $declaredTests++
        }
    }
    if ($declaredTests -ne $tests.Count) {
        throw 'Discovered tests do not match the tests declared in requirement folders.'
    }
    foreach ($test in $tests) {
        $owners = @($groups | Where-Object { $test.Contains($_.filter) })
        if ($owners.Count -ne 1) {
            throw "Test must belong to exactly one requirement group: $test"
        }
    }
}

function Invoke-SensitivityChecks {
    $scriptPath = Join-Path $PSScriptRoot 'check-tests-catch-bugs.py'
    if (Get-Command python -ErrorAction SilentlyContinue) {
        python $scriptPath
    } elseif (Get-Command python3 -ErrorAction SilentlyContinue) {
        python3 $scriptPath
    } elseif (Get-Command py -ErrorAction SilentlyContinue) {
        py -3 $scriptPath
    } else {
        throw 'Python 3 is required for the deliberate-bug checks.'
    }
    if ($LASTEXITCODE -ne 0) {
        throw 'Tests did not catch every deliberate bug.'
    }
}

function Invoke-RequestedChecks {
    if ($Requirement) {
        $group = $groups | Where-Object { $_.requirements -contains $Requirement }
        Write-Output "R${Requirement}: $($group.suite)"
        cargo test -p server $group.filter
        if ($LASTEXITCODE -ne 0) {
            throw "R$Requirement failed."
        }
        if ($Requirement -eq 19) {
            cargo build --release -p server
            if ($LASTEXITCODE -ne 0) {
                throw 'Release build failed.'
            }
        }
    } else {
        cargo fmt -p server -- --check
        if ($LASTEXITCODE -ne 0) {
            throw 'Formatting failed.'
        }
        cargo clippy -p server --all-targets --no-deps -- -D warnings
        if ($LASTEXITCODE -ne 0) {
            throw 'Clippy failed.'
        }
        cargo test -p server
        if ($LASTEXITCODE -ne 0) {
            throw 'Debug tests failed.'
        }
        cargo test --release -p server
        if ($LASTEXITCODE -ne 0) {
            throw 'Release tests failed.'
        }
        Invoke-SensitivityChecks
    }
}

Push-Location -LiteralPath $projectPath
try {
    $tests = @(Get-RegisteredTests)
    Assert-RequirementLayout -groups $groups -tests $tests
    Write-Output "Requirement map checked: R1-R20; $($tests.Count) registered tests."
    Invoke-RequestedChecks
    Write-Output 'PASS: all requested checks completed.'
} finally {
    Pop-Location
}
