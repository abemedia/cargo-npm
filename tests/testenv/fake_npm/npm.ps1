param(
    [string]$Command,
    [string]$Arg2,
    [Parameter(ValueFromRemainingArguments)]
    [string[]]$Rest
)

function Get-PkgInfo($Arg) {
    try {
        if ($Arg -match '\.tgz$') {
            $json = (tar -xzOf $Arg package/package.json) | ConvertFrom-Json
        } else {
            $json = Get-Content -Raw "$Arg\package.json" | ConvertFrom-Json
        }
        if (-not $json.name -or -not $json.version) { return $null }
        return @{ Name = $json.name; Version = $json.version }
    } catch {
        return $null
    }
}

switch ($Command) {
    '--version' {
        Write-Output '0.0.0-fake'
        exit 0
    }
    'view' {
        $log = $env:FAKE_NPM_LOG
        if (Test-Path $log) {
            foreach ($line in Get-Content $log -Encoding UTF8) {
                $parts = $line -split "`t"
                if ("$($parts[0])@$($parts[1])" -eq $Arg2) {
                    Write-Output $parts[1]
                    exit 0
                }
            }
        }
        Write-Error 'npm error code E404'
        exit 1
    }
    'publish' {
        $log = $env:FAKE_NPM_LOG
        $info = Get-PkgInfo $Arg2
        if ($null -eq $info) {
            Write-Error "npm error: could not read package metadata from $Arg2"
            exit 1
        }
        $spec = "$($info.Name)@$($info.Version)"
        if (Test-Path $log) {
            foreach ($line in Get-Content $log -Encoding UTF8) {
                $parts = $line -split "`t"
                if ("$($parts[0])@$($parts[1])" -eq $spec) {
                    Write-Error 'npm error: cannot publish over the previously published versions.'
                    exit 1
                }
            }
        }
        $entry = "$($info.Name)`t$($info.Version)`t$($Rest -join ' ')"
        Add-Content $log $entry
        exit 0
    }
}
