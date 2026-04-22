param(
    [Parameter(Mandatory = $true)]
    [string]$Version,
    [switch]$PushTag,
    [switch]$SkipBuildValidation
)

$ErrorActionPreference = "Stop"

if (-not ($Version -match '^v\d+\.\d+\.\d+([\-+][0-9A-Za-z\.-]+)?$')) {
    throw "Version must look like vMAJOR.MINOR.PATCH (example: v0.1.0). Got: $Version"
}

Write-Host "Preparing release for $Version" -ForegroundColor Cyan

$status = git status --porcelain
if ($status) {
    throw "Working tree is not clean. Please commit or stash changes before releasing."
}

$existingTag = git tag --list $Version
if ($existingTag) {
    throw "Tag already exists: $Version"
}

if (-not $SkipBuildValidation) {
    Write-Host "Running release prechecks (UI + VS Code extension)..." -ForegroundColor Cyan
    cargo check

    Push-Location "vscode-extension"
    try {
        npm ci
        npm run compile
        $tmpVsix = Join-Path $env:TEMP "cligj-$Version-local-check.vsix"
        if (Test-Path $tmpVsix) {
            Remove-Item $tmpVsix -Force
        }
        npx @vscode/vsce package --no-yarn --out $tmpVsix
        if (Test-Path $tmpVsix) {
            Remove-Item $tmpVsix -Force
        }
    }
    finally {
        Pop-Location
    }
}

git tag -a $Version -m "Release $Version"
Write-Host "Created annotated tag $Version" -ForegroundColor Green

if ($PushTag) {
    git push origin $Version
    Write-Host "Pushed tag $Version to origin. GitHub Release workflow should start automatically." -ForegroundColor Green
} else {
    Write-Host "Tag created locally. Run 'git push origin $Version' to trigger GitHub release workflow." -ForegroundColor Yellow
}
