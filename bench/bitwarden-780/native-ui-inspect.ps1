# Read-only inspection of a disposable CI fixture, never a user browser.
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true') { throw 'Native UI inspection is CI-only' }
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
$request = [Console]::In.ReadToEnd() | ConvertFrom-Json
$root = [System.Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$request.hwnd)
$current = $root.Current
if ($current.ProcessId -ne [int]$request.pid -or -not $current.Name.StartsWith([string]$request.title, [StringComparison]::Ordinal)) {
    throw 'The native fixture window identity changed'
}
$condition = [System.Windows.Automation.PropertyCondition]::new(
    [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
    [System.Windows.Automation.ControlType]::Document)
$elements = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condition)
$documents = @()
$failures = @()
for ($index = 0; $index -lt [Math]::Min($elements.Count, 256); $index++) {
    $stage = 'current-properties'
    try {
        $element = $elements[$index]
        $properties = $element.Current
        $document = @{name=$properties.Name; offscreen=$properties.IsOffscreen;
            controlType=$properties.ControlType.ProgrammaticName; className=$properties.ClassName}
        $documents += $document
        $stage = 'document-url-property'
        # Query the Document's legacy URL property directly. PowerShell's
        # untyped pattern wrapper may not expose the concrete pattern's Current.
        # Never query an Edit/Password element or enumerate its values.
        $value = $element.GetCurrentPropertyValue(
            [System.Windows.Automation.LegacyIAccessiblePattern]::ValueProperty, $true)
        $supported = -not [object]::ReferenceEquals(
            $value, [System.Windows.Automation.AutomationElement]::NotSupported)
        $document.urlSupported = $supported
        $document.url = if ($supported) { [string]$value } else { $null }
    } catch {
        $failures += @{index=$index;stage=$stage;error=$_.Exception.Message}
    }
}
@{readOnly=$true;pid=$current.ProcessId;hwnd=$current.NativeWindowHandle;
    documentCount=$elements.Count;truncated=($elements.Count -gt 256);
    failedDocuments=$failures.Count;failures=$failures;documents=$documents} |
    ConvertTo-Json -Depth 6 -Compress
