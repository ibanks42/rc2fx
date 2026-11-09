# Write the Azure Artifact Signing metadata vpk signs Windows packages with,
# from repository variables (not secrets: azure/login signs in through
# GitHub's OIDC token). With none set, the Windows packages stay unsigned and
# the run says so. Sets RC2FX_ARTIFACT_SIGNING_METADATA and output `enabled`.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$names = @('AZURE_TENANT_ID', 'AZURE_CLIENT_ID',
    'ARTIFACT_SIGNING_ENDPOINT', 'ARTIFACT_SIGNING_ACCOUNT', 'ARTIFACT_SIGNING_PROFILE')
$set = @($names | Where-Object { [Environment]::GetEnvironmentVariable($_) })
if ($set.Count -eq 0) {
    Write-Host '::warning::No Azure Artifact Signing settings: Windows users will see an unknown-publisher warning.'
    'enabled=false' | Out-File -FilePath $env:GITHUB_OUTPUT -Append -Encoding utf8
    exit 0
}
if ($set.Count -ne $names.Count) {
    Write-Host "::error::Set all of $($names -join ' '), or none."
    exit 1
}

$metadata = Join-Path $env:RUNNER_TEMP artifact-signing.json
# Sign in only through the Azure CLI session azure/login leaves behind.
@{
    Endpoint               = $env:ARTIFACT_SIGNING_ENDPOINT
    CodeSigningAccountName = $env:ARTIFACT_SIGNING_ACCOUNT
    CertificateProfileName = $env:ARTIFACT_SIGNING_PROFILE
    ExcludeCredentials     = @(
        'EnvironmentCredential', 'ManagedIdentityCredential', 'WorkloadIdentityCredential',
        'SharedTokenCacheCredential', 'VisualStudioCredential', 'VisualStudioCodeCredential',
        'AzurePowerShellCredential', 'AzureDeveloperCliCredential', 'InteractiveBrowserCredential')
} | ConvertTo-Json | Out-File -FilePath $metadata -Encoding utf8NoBOM

"RC2FX_ARTIFACT_SIGNING_METADATA=$metadata" | Out-File -FilePath $env:GITHUB_ENV -Append -Encoding utf8
'enabled=true' | Out-File -FilePath $env:GITHUB_OUTPUT -Append -Encoding utf8
Write-Host "Artifact Signing metadata: $metadata"
