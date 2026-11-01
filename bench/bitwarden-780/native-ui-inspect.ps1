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
$failures = 0
for ($index = 0; $index -lt [Math]::Min($elements.Count, 256); $index++) {
    try {
        $element = $elements[$index]
        $properties = $element.Current
        $pattern = $null
        $url = $null
        if ($element.TryGetCurrentPattern([System.Windows.Automation.LegacyIAccessiblePattern]::Pattern, [ref]$pattern)) {
            # Only Document values are read. No Edit/Password value pattern.
            $url = $pattern.Current.Value
        }
        $documents += @{name=$properties.Name; url=$url; offscreen=$properties.IsOffscreen;
            controlType=$properties.ControlType.ProgrammaticName; className=$properties.ClassName}
    } catch { $failures++ }
}
@{readOnly=$true;pid=$current.ProcessId;hwnd=$current.NativeWindowHandle;
    documentCount=$elements.Count;truncated=($elements.Count -gt 256);failedDocuments=$failures;documents=$documents} |
    ConvertTo-Json -Depth 6 -Compress
