#requires -Version 5.1

[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidateSet('Check', 'Install', 'Uninstall')]
    [string] $Action,

    [Parameter(Mandatory)]
    [ValidateSet('CurrentUser', 'AllUsers')]
    [string] $Scope,

    [Parameter(Mandatory)]
    [ValidateNotNullOrEmpty()]
    [string] $InstallDir,

    [switch] $AddPath,
    [switch] $EnableAlias,
    [switch] $RemovePath,
    [switch] $RemoveAlias
)

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'

$script:BlockStart = '# >>> FW installer integration >>>'
$script:BlockEnd = '# <<< FW installer integration <<<'

function Get-PathTarget {
    if ($Scope -eq 'AllUsers') { return 'Machine' }
    return 'User'
}

function Get-ProfilePaths {
    $documents = [Environment]::GetFolderPath([Environment+SpecialFolder]::MyDocuments)
    @(
        [IO.Path]::Combine($documents, 'WindowsPowerShell', 'Microsoft.PowerShell_profile.ps1')
        [IO.Path]::Combine($documents, 'PowerShell', 'Microsoft.PowerShell_profile.ps1')
    )
}

function Normalize-PathEntry {
    param([Parameter(Mandatory)][string] $Value)
    return $Value.Trim().TrimEnd('\')
}

function Test-SamePath {
    param(
        [Parameter(Mandatory)][string] $Left,
        [Parameter(Mandatory)][string] $Right
    )
    return [string]::Equals(
        (Normalize-PathEntry $Left),
        (Normalize-PathEntry $Right),
        [StringComparison]::OrdinalIgnoreCase
    )
}

function Get-PathEntries {
    param([AllowEmptyString()][string] $Value)
    if ([string]::IsNullOrWhiteSpace($Value)) { return @() }
    return @($Value.Split(';') | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
}

function Find-FwExecutableCollision {
    $pathValue = [Environment]::GetEnvironmentVariable('Path', 'Process')
    foreach ($entry in (Get-PathEntries $pathValue)) {
        foreach ($fileName in @('fw.com', 'fw.exe', 'fw.bat', 'fw.cmd', 'fw.ps1')) {
            $candidate = [IO.Path]::Combine($entry.Trim().Trim('"'), $fileName)
            if ([IO.File]::Exists($candidate)) {
                $expected = [IO.Path]::Combine($InstallDir, 'fw.exe')
                if (-not (Test-SamePath $candidate $expected)) { return $candidate }
            }
        }
    }
    return $null
}

function Remove-ManagedBlock {
    param([AllowEmptyString()][string] $Content)
    $pattern = '(?ms)^' + [regex]::Escape($script:BlockStart) +
        '.*?^' + [regex]::Escape($script:BlockEnd) + '(?:\r?\n)?'
    return [regex]::Replace($Content, $pattern, '')
}

function Test-CustomFwProfileCommand {
    foreach ($profilePath in (Get-ProfilePaths)) {
        if (-not [IO.File]::Exists($profilePath)) { continue }
        $content = Remove-ManagedBlock ([IO.File]::ReadAllText($profilePath))
        $patterns = @(
            '(?im)^\s*function\s+(?:(?:global|script|local):)?fw\b',
            '(?im)^\s*(?:Set-Alias|New-Alias)\b[^\r\n]*(?:-Name\s+)?["'']?fw["'']?(?:\s|$)',
            '(?im)^\s*(?:Remove-Item|Remove-Variable)\b[^\r\n]*(?:Alias:fw|\bfw\b)'
        )
        foreach ($pattern in $patterns) {
            if ($content -match $pattern) { return $true }
        }
    }
    return $false
}

function Test-ProfileWritable {
    foreach ($profilePath in (Get-ProfilePaths)) {
        if ([IO.File]::Exists($profilePath)) {
            try {
                $stream = [IO.File]::Open($profilePath, 'Open', 'ReadWrite', 'Read')
                $stream.Dispose()
            } catch {
                return $false
            }
        } else {
            $parent = [IO.Path]::GetDirectoryName($profilePath)
            while (-not [string]::IsNullOrEmpty($parent) -and -not [IO.Directory]::Exists($parent)) {
                $parent = [IO.Path]::GetDirectoryName($parent)
            }
            if ([string]::IsNullOrEmpty($parent)) { return $false }
            try {
                $probe = [IO.Path]::Combine($parent, [IO.Path]::GetRandomFileName())
                [IO.File]::WriteAllText($probe, '')
                [IO.File]::Delete($probe)
            } catch {
                return $false
            }
        }
    }
    return $true
}

function Set-FwPathEntry {
    param([bool] $Present)
    $target = Get-PathTarget
    $current = [Environment]::GetEnvironmentVariable('Path', $target)
    $entries = @(Get-PathEntries $current)
    $filtered = @($entries | Where-Object { -not (Test-SamePath $_ $InstallDir) })

    if ($Present) { $filtered += $InstallDir }
    $updated = $filtered -join ';'
    [Environment]::SetEnvironmentVariable('Path', $updated, $target)

    if (-not ('FW.NativeMethods' -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
namespace FW {
    public static class NativeMethods {
        [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        public static extern IntPtr SendMessageTimeout(
            IntPtr hWnd, uint Msg, UIntPtr wParam, string lParam,
            uint fuFlags, uint uTimeout, out UIntPtr lpdwResult);
    }
}
'@
    }
    $result = [UIntPtr]::Zero
    [void][FW.NativeMethods]::SendMessageTimeout(
        [IntPtr]0xffff, 0x001A, [UIntPtr]::Zero, 'Environment',
        0x0002, 5000, [ref]$result
    )
}

function Set-FwProfileIntegration {
    param([bool] $Present)

    $blockBody = @'
# This block replaces PowerShell's `fw` alias for Format-Wide so that
# `fw` invokes FW directly, without requiring `fw.exe`.
# It is managed automatically by the FW installer.

$fwAlias = Get-Alias -Name fw -ErrorAction SilentlyContinue
if ($null -ne $fwAlias -and $fwAlias.Definition -eq 'Format-Wide') {
    Remove-Item Alias:fw -Force -ErrorAction SilentlyContinue
}
'@
    $block = $script:BlockStart + "`r`n" + $blockBody.Trim() + "`r`n" + $script:BlockEnd

    foreach ($profilePath in (Get-ProfilePaths)) {
        $content = if ([IO.File]::Exists($profilePath)) {
            [IO.File]::ReadAllText($profilePath)
        } else {
            ''
        }
        $content = (Remove-ManagedBlock $content).TrimEnd("`r", "`n")

        if ($Present) {
            $parent = [IO.Path]::GetDirectoryName($profilePath)
            [IO.Directory]::CreateDirectory($parent) | Out-Null
            if ($content.Length -gt 0) { $content += "`r`n`r`n" }
            $content += $block + "`r`n"
            [IO.File]::WriteAllText($profilePath, $content, [Text.UTF8Encoding]::new($false))
        } elseif ([IO.File]::Exists($profilePath)) {
            if ($content.Length -gt 0) { $content += "`r`n" }
            [IO.File]::WriteAllText($profilePath, $content, [Text.UTF8Encoding]::new($false))
        }
    }
}

switch ($Action) {
    'Check' {
        $collision = Find-FwExecutableCollision
        if ($null -ne $collision) {
            Write-Output "collision=$collision"
            exit 2
        }
        if ((Test-CustomFwProfileCommand) -or -not (Test-ProfileWritable)) {
            Write-Output 'alias=unavailable'
            exit 1
        }
        Write-Output 'alias=available'
        exit 0
    }
    'Install' {
        if ($AddPath) { Set-FwPathEntry $true }
        if ($EnableAlias) { Set-FwProfileIntegration $true }
    }
    'Uninstall' {
        if ($RemovePath) { Set-FwPathEntry $false }
        if ($RemoveAlias) { Set-FwProfileIntegration $false }
    }
}
