# Install VS 2022 Build Tools with the C++ workload (MSVC linker + headers)
# Using --override to pass args directly to the VS installer.
# Single quotes in PowerShell preserve the string as one argument.
winget install Microsoft.VisualStudio.2022.BuildTools `
  --override '--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended' `
  --accept-package-agreements `
  --accept-source-agreements

Write-Host "winget exit code: $LASTEXITCODE"
