Output demo.gif
Require chrome-use
Set Shell "bash"
Set FontSize 16
Set Width 1360
Set Height 380
Set Padding 22
Set Theme "Catppuccin Mocha"
Set TypingSpeed 34ms
Set Framerate 24

Sleep 500ms
Type "# chrome-use: drive your REAL, logged-in Chrome from any AI agent"
Sleep 500ms
Enter
Sleep 300ms

Type "chrome-use open https://news.ycombinator.com 2>/dev/null"
Sleep 300ms
Enter
Sleep 2200ms

Type "# one command -> structured data, straight from your real browser"
Sleep 450ms
Enter
Sleep 150ms

Type "chrome-use site hackernews/top --json 2>/dev/null | jq -c '.data.result.posts[:3][] | {rank, title: .title[:44], score}'"
Sleep 400ms
Enter
Sleep 3800ms
Sleep 1400ms
