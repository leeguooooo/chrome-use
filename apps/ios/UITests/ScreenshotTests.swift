import XCTest

/// Pairs the app in the simulator with a real `chrome-use mobile serve`
/// (fake panel, demo data) and walks the main screens, saving screenshots.
/// Elements are found by accessibility identifier, so the same walk runs in
/// every language. The runner script sets TEST_RUNNER_PAIR_LINK,
/// TEST_RUNNER_SHOTS_DIR and TEST_RUNNER_SHOTS_LANGUAGE.
final class ScreenshotTests: XCTestCase {
    var app: XCUIApplication!
    var shots: URL?
    var language = "en"

    static let locales = ["en": "en_US", "zh-Hans": "zh_CN", "zh-Hant": "zh_TW", "ja": "ja_JP"]

    override func setUp() {
        continueAfterFailure = false
        let env = ProcessInfo.processInfo.environment
        shots = env["SHOTS_DIR"].map { URL(fileURLWithPath: $0) }
        language = env["SHOTS_LANGUAGE"] ?? "en"
        app = XCUIApplication()
        app.launchArguments += ["-AppleLanguages", "(\(language))", "-AppleLocale", Self.locales[language] ?? "en_US"]
        app.launchEnvironment["CHROMEUSE_UITEST_TRIAL"] = "1"
        if let worker = env["PUSH_WORKER"] { app.launchArguments += ["-pushWorkerURL", worker] }
    }

    func snap(_ name: String) {
        let shot = XCUIScreen.main.screenshot()
        let attachment = XCTAttachment(screenshot: shot)
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
        if let shots {
            try? FileManager.default.createDirectory(at: shots, withIntermediateDirectories: true)
            try? shot.pngRepresentation.write(to: shots.appendingPathComponent("\(language)-\(name).png"))
        }
    }

    func element(_ id: String) -> XCUIElement {
        app.descendants(matching: .any).matching(identifier: id).firstMatch
    }

    /// The notification permission alert belongs to SpringBoard; its
    /// default (allow) button is the second one in every language.
    func allowNotifications() {
        let alert = XCUIApplication(bundleIdentifier: "com.apple.springboard").alerts.firstMatch
        if alert.waitForExistence(timeout: 5), alert.buttons.count >= 2 { alert.buttons.element(boundBy: 1).tap() }
    }

    func back() {
        app.navigationBars.buttons.element(boundBy: 0).tap()
    }

    func testWalkthrough() throws {
        let link = try XCTUnwrap(ProcessInfo.processInfo.environment["PAIR_LINK"], "PAIR_LINK is set by the runner")

        app.launch()
        XCTAssertTrue(element("noComputers").waitForExistence(timeout: 30))
        snap("01-empty")

        // The scanned / opened link fills the pairing sheet.
        app.open(URL(string: link)!)
        let pair = app.buttons["pairButton"]
        XCTAssertTrue(pair.waitForExistence(timeout: 30))
        snap("02-pairing")
        pair.tap()
        allowNotifications()

        // The demo content is in the walkthrough's language; the waiting
        // conversation's row is found by its phase.
        let weekly = app.buttons["phase-waitingInput"]
        XCTAssertTrue(weekly.waitForExistence(timeout: 40), "conversation list after pairing")
        snap("03-conversations")

        weekly.tap()
        let allow = app.buttons[#"decision-"allow""#]
        XCTAssertTrue(allow.waitForExistence(timeout: 30), "approval card")
        sleep(1)
        snap("04-approval")
        allow.tap()
        // Answered: the service resolves the request and its card goes away.
        let gone = expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: allow)
        wait(for: [gone], timeout: 10)
        sleep(1)
        snap("05-approved")

        back()
        let compose = app.buttons["newConversation"]
        XCTAssertTrue(compose.waitForExistence(timeout: 20))
        compose.tap()
        XCTAssertTrue(app.buttons["cancel"].waitForExistence(timeout: 30))
        sleep(2)
        snap("06-new-conversation")
        app.buttons["cancel"].tap()

        app.buttons["schedules"].tap()
        XCTAssertTrue(app.switches.firstMatch.waitForExistence(timeout: 30), "a schedule with its toggle")
        snap("07-schedules")
        back()
        back()

        XCTAssertTrue(app.buttons["settings"].waitForExistence(timeout: 20))
        snap("08-computers")
        app.buttons["settings"].tap()
        XCTAssertTrue(app.buttons["buyOrTry"].waitForExistence(timeout: 20))
        snap("09-settings")
        app.buttons["buyOrTry"].tap()
        XCTAssertTrue(element("paywallTitle").waitForExistence(timeout: 20))
        sleep(2)
        snap("10-paywall")
    }
}
