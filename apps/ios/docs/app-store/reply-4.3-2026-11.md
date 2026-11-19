# Reply to App Review: 4.3 / 4.2.6 (submission 5e2140d8, sent 2026-11-19)

Hello App Review,

Thank you for the review. We believe chrome-use Remote meets guidelines 4.3(a), 4.3(b) and 4.2.6, so we are answering your questions here instead of resubmitting. Build 1.0 (2) is attached to this version.

1. What the app does. chrome-use is our open-source tool (https://github.com/leeguooooo/chrome-use, Apache-2.0) that lets AI coding agents such as Claude Code and Codex work in the user's real, signed-in Chrome on their own computer. Its desktop "agent panel" runs conversations with those agents and stops to ask the user before sensitive actions (sending, buying, deleting, entering personal data). chrome-use Remote is the phone client for that panel. The user pairs it with their computer by scanning a QR code (`chrome-use mobile pair`). Then they can start a conversation, follow the agent live, see its browser tab, answer its approvals and questions, and run or pause scheduled tasks. The phone connects directly to the user's computer over their local network, Tailscale or their own Cloudflare tunnel. Everything, including push notifications, is end-to-end encrypted between phone and computer. The problem it solves: a long agent task stalls whenever it needs a human approval and the human has walked away from the desk. With the app, the approval reaches them on the phone.

2. Intended users. Developers and technical users who already run chrome-use on a Mac, Windows or Linux computer to have AI agents do browser work for them, such as filling forms, checking dashboards or running scheduled checks. The app is useless without chrome-use, and the product page says so.

3. The gap. No other app controls chrome-use. Remote-desktop apps stream the whole screen, which is impractical on a phone and can.t present an agent.s approval requests as such. chrome-use Remote shows the agent's conversation, pending approvals and scheduled tasks as native screens. It also keeps the security boundary on the computer: conversations started from the phone cannot run shell commands, and full access can only be turned on at the desk.

4. Beta testing. We did not run a public beta. The developer tested the app on our own iPhones against chrome-use on our own computers, and through an internal TestFlight group. Automated tests cover the protocol and crypto, including an end-to-end test against a real gateway. Testing between builds 1 and 2 found camera permission and QR-scanner lifecycle bugs during pairing, and build 2 fixes them.

5. Standalone or suite. It is the companion of one desktop product, chrome-use. The closest app on our account is Phone Use Remote (already on the App Store). That is the remote for a different open-source tool of ours, iphone-use, which lets agents operate a physical iPhone connected to a Mac. Phone Use Remote shows that iPhone's live screen and lets the user take over with taps, swipes and typing. chrome-use Remote has no screen streaming or touch control. It shows an agent conversation in a browser, with approvals, notifications and schedules. They talk to different desktop programs over different protocols. Our other apps are unrelated.

6. Could it be an in-app purchase in another app? No. It would only make sense inside Phone Use Remote, and that app's job is driving a phone screen for a different desktop tool. A chrome-use client inside an "iPhone remote" app would confuse users of both.

7. Shared code or assets with our other apps. No. The app and its ChromeUseKit package (connection, handshake, encryption, pairing) were written for this app. They share no code, frameworks or assets with Phone Use Remote or any other app of ours.

8. Shared codebase with third parties. No. It uses Apple frameworks only (SwiftUI, StoreKit 2, CryptoKit, LocalAuthentication, AVFoundation and VisionKit for the QR scanner).

9. Made for a client? No. chrome-use and this app are our own products, not made from a template or an app generation service.

Thank you,
Li Guo
