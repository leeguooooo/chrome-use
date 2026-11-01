# Read only native Document metadata for an already-bound Chrome window.
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
# The framework UIAutomationClient assembly does not expose the native legacy
# value property. Use the Windows SDK COM interface prefixes directly instead.
# Source: microsoft/win32metadata, UIAutomationClient.h (IUIAutomation,
# IUIAutomationElement, IUIAutomationTreeWalker). No mutation method is called.
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
namespace FixtureUia {
    [StructLayout(LayoutKind.Sequential)] public struct Point { public int X, Y; }
    [ComImport, Guid("30cbe57d-d9d0-452a-ab13-7ac5ac4825ee"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface Automation {
        [return: MarshalAs(UnmanagedType.Bool)] bool CompareElements(Element a, Element b);
        [return: MarshalAs(UnmanagedType.Bool)] bool CompareRuntimeIds([MarshalAs(UnmanagedType.SafeArray)] int[] a, [MarshalAs(UnmanagedType.SafeArray)] int[] b);
        Element GetRootElement();
        Element ElementFromHandle(IntPtr hwnd);
        Element ElementFromPoint(Point pt);
        Element GetFocusedElement();
        Element GetRootElementBuildCache(IntPtr request);
        Element ElementFromHandleBuildCache(IntPtr hwnd, IntPtr request);
        Element ElementFromPointBuildCache(Point pt, IntPtr request);
        Element GetFocusedElementBuildCache(IntPtr request);
        Walker CreateTreeWalker(IntPtr condition);
        Walker GetControlViewWalker();
        Walker GetContentViewWalker();
        Walker GetRawViewWalker();
    }
    [ComImport, Guid("d22108aa-8ac5-49a5-837b-37bbb3d7591e"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface Element {
        void SetFocus();
        [return: MarshalAs(UnmanagedType.SafeArray)] int[] GetRuntimeId();
        Element FindFirst(int scope, IntPtr condition);
        IntPtr FindAll(int scope, IntPtr condition);
        Element FindFirstBuildCache(int scope, IntPtr condition, IntPtr request);
        IntPtr FindAllBuildCache(int scope, IntPtr condition, IntPtr request);
        Element BuildUpdatedCache(IntPtr request);
        [return: MarshalAs(UnmanagedType.Struct)] object GetCurrentPropertyValue(int id);
        [return: MarshalAs(UnmanagedType.Struct)] object GetCurrentPropertyValueEx(int id, [MarshalAs(UnmanagedType.Bool)] bool ignoreDefaultValue);
    }
    [ComImport, Guid("4042c624-389c-4afc-a630-9df854a541fc"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface Walker {
        Element GetParentElement(Element element);
        Element GetFirstChildElement(Element element);
        Element GetLastChildElement(Element element);
        Element GetNextSiblingElement(Element element);
    }
    class Node {
        public Element Element;
        public string RootUrl;
        public Node(Element element, string rootUrl) { Element = element; RootUrl = rootUrl; }
    }
    public static class Inspector {
        // SDK property/control identifiers. Only Document legacy values (URLs)
        // are queried; Edit/Password values and text patterns are never read.
        const int ProcessId = 30002, ControlType = 30003, Name = 30005;
        const int ClassName = 30012, IsOffscreen = 30022;
        const int LegacyValue = 30093, Document = 50030, Limit = 2048;
        public static object Read(long handle, int pid) {
            var com = Activator.CreateInstance(Type.GetTypeFromCLSID(new Guid("ff48dba4-60ef-4201-aa87-54103eef594e")));
            try {
                var automation = (Automation)com;
                var root = automation.ElementFromHandle(new IntPtr(handle));
                // The native caller binds HWND/process ownership; document URLs
                // below bind its page. Accessible names are presentation text.
                if (Convert.ToInt32(root.GetCurrentPropertyValue(ProcessId)) != pid)
                    throw new InvalidOperationException("The native window identity changed");
                var walker = automation.GetRawViewWalker();
                var pending = new Stack<Node>();
                pending.Push(new Node(root, null));
                var documents = new List<object>();
                var failures = new List<object>();
                int visited = 0;
                while (pending.Count > 0 && visited < Limit) {
                    var node = pending.Pop();
                    var element = node.Element;
                    var rootUrl = node.RootUrl;
                    int index = visited++;
                    string stage = "control-type";
                    try {
                        if (Convert.ToInt32(element.GetCurrentPropertyValue(ControlType)) == Document) {
                            stage = "document-properties";
                            var document = new Dictionary<string, object> {
                                {"name", element.GetCurrentPropertyValue(Name)},
                                {"className", element.GetCurrentPropertyValue(ClassName)},
                                {"offscreen", element.GetCurrentPropertyValue(IsOffscreen)},
                                {"controlType", "ControlType.Document"}
                            };
                            documents.Add(document);
                            stage = "document-url-property";
                            var value = element.GetCurrentPropertyValueEx(LegacyValue, true);
                            document["urlSupported"] = value is string;
                            document["url"] = value as string;
                            Uri url;
                            if (rootUrl == null && value is string && Uri.TryCreate((string)value, UriKind.Absolute, out url) &&
                                (url.Scheme == "http" || url.Scheme == "https")) rootUrl = (string)value;
                            document["rootUrl"] = rootUrl;
                        }
                        stage = "children";
                        for (var child = walker.GetFirstChildElement(element); child != null; child = walker.GetNextSiblingElement(child)) {
                            if (pending.Count + visited >= Limit) break;
                            pending.Push(new Node(child, rootUrl));
                        }
                    } catch (Exception error) {
                        failures.Add(new {index, stage, error = error.Message});
                    }
                }
                return new {readOnly = true, pid, hwnd = handle, documentCount = documents.Count,
                    visitedNodes = visited, truncated = visited >= Limit, failedDocuments = failures.Count,
                    failures, documents};
            } finally { Marshal.FinalReleaseComObject(com); }
        }
    }
}
'@
$request = [Console]::In.ReadToEnd() | ConvertFrom-Json
# The first observed native snapshots contained browser chrome but no web
# Document. Keep each re-read bounded and retain the empty attempts as facts.
$attempts = @()
for ($attempt = 0; $attempt -lt 8; $attempt++) {
    $snapshot = [FixtureUia.Inspector]::Read([long]$request.hwnd, [int]$request.pid)
    $attempts += @{documentCount=$snapshot.documentCount;visitedNodes=$snapshot.visitedNodes;
        failedDocuments=$snapshot.failedDocuments;truncated=$snapshot.truncated}
    $pageDocuments = @($snapshot.documents | Where-Object {
        $_.url -is [string] -and $_.url -ceq [string]$request.url
    })
    $menuDocuments = @($snapshot.documents | Where-Object {
        $_.url -is [string] -and $_.url -match '^chrome-extension://(nngceckbapebfimnlniiiahkandclblb|hmjfbpicbfcphbcmemmcghpkcdinoahh)/' -and
        $_.url -match '/overlay/menu(-button|-list)?[.]html([?#]|$)'
    })
    # Raw-view browser widgets can themselves be Document controls with empty
    # values. Their count must never stand in for the fixture or menu identity.
    if (($pageDocuments.Count -gt 0 -and $menuDocuments.Count -gt 0) -or $snapshot.truncated -or $snapshot.failedDocuments -gt 0) { break }
    if ($attempt -lt 7) { Start-Sleep -Milliseconds 150 }
}
@{readOnly=$snapshot.readOnly;pid=$snapshot.pid;hwnd=$snapshot.hwnd;
    documentCount=$snapshot.documentCount;visitedNodes=$snapshot.visitedNodes;
    truncated=$snapshot.truncated;failedDocuments=$snapshot.failedDocuments;
    failures=$snapshot.failures;documents=$snapshot.documents;attempts=$attempts} |
    ConvertTo-Json -Depth 8 -Compress
