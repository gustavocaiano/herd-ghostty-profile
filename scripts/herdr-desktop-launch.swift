import AppKit
import Foundation

private let bundleIDFamilyPrefix = "com.gustavocaiano.herdr"

enum CLIError: Error, CustomStringConvertible {
    case usage(String)
    case stale(String)
    case unavailable(String)
    case software(String)
    case temporary(String)

    var code: Int32 {
        switch self {
        case .usage: 64
        case .stale: 65
        case .unavailable: 69
        case .software: 70
        case .temporary: 75
        }
    }

    var description: String {
        switch self {
        case .usage(let message), .stale(let message), .unavailable(let message),
             .software(let message), .temporary(let message): message
        }
    }
}

struct Identity {
    let pid: pid_t
    let launchDateUnixMS: Int64
    let bundleID: String
    let bundlePath: String
}

final class LaunchResult: @unchecked Sendable {
    private let lock = NSLock()
    private var application: NSRunningApplication?
    private var errorMessage: String?

    func store(application: NSRunningApplication?, error: Error?) {
        lock.lock()
        self.application = application
        errorMessage = error?.localizedDescription
        lock.unlock()
    }

    func value() -> (NSRunningApplication?, String?) {
        lock.lock()
        defer { lock.unlock() }
        return (application, errorMessage)
    }
}

func parseOptions(_ arguments: ArraySlice<String>, allowed: Set<String>) throws -> [String: String] {
    let values = Array(arguments)
    guard values.count.isMultiple(of: 2) else {
        throw CLIError.usage("every option must have exactly one value")
    }
    var parsed: [String: String] = [:]
    var index = 0
    while index < values.count {
        let option = values[index]
        guard allowed.contains(option) else {
            throw CLIError.usage("unknown option \(option.debugDescription)")
        }
        guard parsed[option] == nil else {
            throw CLIError.usage("duplicate option \(option.debugDescription)")
        }
        let value = values[index + 1]
        guard !value.isEmpty else {
            throw CLIError.usage("option \(option) requires a non-empty value")
        }
        parsed[option] = value
        index += 2
    }
    return parsed
}

func requireExactOptions(_ options: [String: String], allowed: Set<String>) throws {
    guard options.count == allowed.count else {
        let missing = allowed.filter { options[$0] == nil }.sorted().joined(separator: ", ")
        throw CLIError.usage("missing required options: \(missing)")
    }
}

func required(_ option: String, in options: [String: String]) throws -> String {
    guard let value = options[option] else {
        throw CLIError.usage("missing required option \(option)")
    }
    return value
}

func canonicalPath(_ value: String) -> String {
    URL(fileURLWithPath: value).standardizedFileURL.resolvingSymlinksInPath().path
}

func requireAbsolutePath(_ value: String, label: String) throws {
    guard value.hasPrefix("/") else {
        throw CLIError.usage("\(label) must be an absolute path")
    }
}

func requireDirectory(_ value: String, label: String) throws {
    try requireAbsolutePath(value, label: label)
    var isDirectory: ObjCBool = false
    guard FileManager.default.fileExists(atPath: value, isDirectory: &isDirectory),
          isDirectory.boolValue else {
        throw CLIError.usage("\(label) is not an existing directory: \(value)")
    }
}

func requireReadableFile(_ value: String, label: String) throws {
    try requireAbsolutePath(value, label: label)
    var isDirectory: ObjCBool = false
    guard FileManager.default.fileExists(atPath: value, isDirectory: &isDirectory),
          !isDirectory.boolValue,
          FileManager.default.isReadableFile(atPath: value) else {
        throw CLIError.usage("\(label) is not a readable file: \(value)")
    }
}

func requireExecutableFile(_ value: String, label: String) throws {
    try requireReadableFile(value, label: label)
    guard FileManager.default.isExecutableFile(atPath: value) else {
        throw CLIError.usage("\(label) is not executable: \(value)")
    }
}

func requireDesktopID(_ value: String) throws {
    let range = NSRange(value.startIndex..<value.endIndex, in: value)
    let expression = try NSRegularExpression(pattern: "^[a-z0-9][a-z0-9_-]{0,63}$")
    guard expression.firstMatch(in: value, range: range)?.range == range else {
        throw CLIError.usage("desktop id must match [a-z0-9][a-z0-9_-]{0,63}")
    }
}

func requireBundleIDFamily(_ value: String) throws {
    guard value == bundleIDFamilyPrefix || value.hasPrefix("\(bundleIDFamilyPrefix).") else {
        throw CLIError.usage(
            "bundle id \(value.debugDescription) must be \(bundleIDFamilyPrefix) or \(bundleIDFamilyPrefix).<suffix>"
        )
    }
}

func launchDateUnixMS(_ date: Date?) throws -> Int64 {
    guard let date else {
        throw CLIError.software("running application has no launch date")
    }
    let milliseconds = Int64((date.timeIntervalSince1970 * 1_000).rounded())
    guard milliseconds > 0 else {
        throw CLIError.software("running application has an invalid launch date")
    }
    return milliseconds
}

func identity(of application: NSRunningApplication) throws -> Identity {
    guard !application.isTerminated, application.processIdentifier > 0 else {
        throw CLIError.stale("application is no longer running")
    }
    guard let bundleID = application.bundleIdentifier,
          let bundleURL = application.bundleURL else {
        throw CLIError.software("running application has no bundle identity")
    }
    return Identity(
        pid: application.processIdentifier,
        launchDateUnixMS: try launchDateUnixMS(application.launchDate),
        bundleID: bundleID,
        bundlePath: canonicalPath(bundleURL.path)
    )
}

func expectedIdentity(_ arguments: ArraySlice<String>) throws -> (NSRunningApplication, Identity) {
    let allowed: Set<String> = ["--pid", "--app", "--bundle-id", "--launch-date-unix-ms"]
    let options = try parseOptions(arguments, allowed: allowed)
    try requireExactOptions(options, allowed: allowed)
    let pidValue = try required("--pid", in: options)
    let appPath = try required("--app", in: options)
    let bundleID = try required("--bundle-id", in: options)
    let launchDateValue = try required("--launch-date-unix-ms", in: options)
    guard let pid = pid_t(pidValue), pid > 0 else {
        throw CLIError.usage("--pid must be a positive process identifier")
    }
    guard let launchDate = Int64(launchDateValue), launchDate > 0 else {
        throw CLIError.usage("--launch-date-unix-ms must be a positive integer")
    }
    try requireDirectory(appPath, label: "Herdr app")
    try requireBundleIDFamily(bundleID)
    guard let application = NSRunningApplication(processIdentifier: pid),
          !application.isTerminated else {
        throw CLIError.stale("no running application exists for pid \(pid)")
    }
    let actual: Identity
    do {
        actual = try identity(of: application)
    } catch {
        throw CLIError.stale("application pid \(pid) has no trustworthy Herdr identity: \(error)")
    }
    let expectedPath = canonicalPath(appPath)
    guard actual.pid == pid,
          actual.bundleID == bundleID,
          actual.bundlePath == expectedPath,
          actual.launchDateUnixMS == launchDate else {
        throw CLIError.stale("application pid \(pid) no longer matches the recorded Herdr identity")
    }
    return (application, actual)
}

func launch(_ arguments: ArraySlice<String>) throws {
    let allowed: Set<String> = [
        "--app", "--bundle-id", "--command", "--switcher-bin", "--config", "--desktop-id",
        "--real-herdr", "--expected-plan",
    ]
    let options = try parseOptions(arguments, allowed: allowed)
    try requireExactOptions(options, allowed: allowed)
    let app = try required("--app", in: options)
    let expectedBundleID = try required("--bundle-id", in: options)
    let command = try required("--command", in: options)
    let switcher = try required("--switcher-bin", in: options)
    let configPath = try required("--config", in: options)
    let desktopID = try required("--desktop-id", in: options)
    let realHerdr = try required("--real-herdr", in: options)
    let expectedPlan = try required("--expected-plan", in: options)

    try requireDirectory(app, label: "Herdr app")
    guard URL(fileURLWithPath: app).pathExtension == "app" else {
        throw CLIError.usage("Herdr app path must end in .app")
    }
    try requireExecutableFile(command, label: "desktop command shim")
    try requireExecutableFile(switcher, label: "desktop switcher binary")
    try requireReadableFile(configPath, label: "desktop configuration")
    try requireExecutableFile(realHerdr, label: "Herdr binary")
    try requireDesktopID(desktopID)
    try requireBundleIDFamily(expectedBundleID)

    let configuration = NSWorkspace.OpenConfiguration()
    configuration.createsNewApplicationInstance = true
    configuration.activates = true
    configuration.addsToRecentItems = false
    configuration.environment = [
        "HERDR_BIN_PATH": command,
        "HERDR_DESKTOP_SWITCHER_BIN": switcher,
        "HERDR_DESKTOPS_TOML": configPath,
        "HERDR_DESKTOP_ID": desktopID,
        "HERDR_DESKTOP_EXPECTED_PLAN": expectedPlan,
        "HERDR_REAL_BIN": realHerdr,
    ]

    let result = LaunchResult()
    let completed = DispatchSemaphore(value: 0)
    NSWorkspace.shared.openApplication(
        at: URL(fileURLWithPath: app),
        configuration: configuration
    ) { application, error in
        result.store(application: application, error: error)
        completed.signal()
    }
    guard completed.wait(timeout: .now() + 20) == .success else {
        throw CLIError.temporary("timed out after 20 seconds while launching Herdr.app")
    }
    let (application, error) = result.value()
    if let error {
        if let application, !terminateAndConfirm(application) {
            throw CLIError.temporary(
                "LaunchServices reported an error and the returned application could not be confirmed terminated: \(error)"
            )
        }
        throw CLIError.software("could not launch Herdr.app: \(error)")
    }
    guard let application else {
        throw CLIError.software("LaunchServices returned no running application")
    }

    Thread.sleep(forTimeInterval: 1.0)
    let actual: Identity
    do {
        actual = try identity(of: application)
    } catch {
        guard terminateAndConfirm(application) else {
            throw CLIError.temporary(
                "launched Herdr.app did not remain stable and could not be confirmed terminated: \(error)"
            )
        }
        throw CLIError.software("launched Herdr.app did not remain stable: \(error)")
    }
    guard actual.bundleID == expectedBundleID,
          actual.bundlePath == canonicalPath(app) else {
        guard terminateAndConfirm(application) else {
            throw CLIError.temporary(
                "launched application identity did not match Herdr.app and termination is unconfirmed"
            )
        }
        throw CLIError.software("launched application identity does not match Herdr.app")
    }
    let object: [String: Any] = [
        "pid": actual.pid,
        "launch_date_unix_ms": actual.launchDateUnixMS,
        "bundle_id": actual.bundleID,
        "bundle_path": actual.bundlePath,
    ]
    let encoded = try JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])
    guard let line = String(data: encoded, encoding: .utf8) else {
        throw CLIError.software("could not encode launch identity")
    }
    print(line)
}

func activate(_ arguments: ArraySlice<String>) throws {
    let (application, identity) = try expectedIdentity(arguments)
    guard application.activate(options: [.activateAllWindows]) else {
        throw CLIError.unavailable("could not activate matching Herdr application pid \(identity.pid)")
    }
}

func terminate(_ arguments: ArraySlice<String>) throws {
    let (application, identity) = try expectedIdentity(arguments)
    guard terminateAndConfirm(application) else {
        throw CLIError.unavailable(
            "could not confirm termination of matching Herdr application pid \(identity.pid)"
        )
    }
}

func terminateAndConfirm(_ application: NSRunningApplication) -> Bool {
    if application.isTerminated {
        return true
    }
    guard application.terminate() else {
        return false
    }
    let deadline = Date().addingTimeInterval(5)
    while !application.isTerminated && Date() < deadline {
        Thread.sleep(forTimeInterval: 0.05)
    }
    return application.isTerminated
}

func run() throws {
    let arguments = CommandLine.arguments.dropFirst()
    guard let command = arguments.first else {
        throw CLIError.usage("usage: herdr-desktop-launch <launch|activate|terminate> [options]")
    }
    switch command {
    case "launch": try launch(arguments.dropFirst())
    case "activate": try activate(arguments.dropFirst())
    case "terminate": try terminate(arguments.dropFirst())
    default:
        throw CLIError.usage(
            "unknown command \(command.debugDescription); expected launch, activate, or terminate"
        )
    }
}

do {
    try run()
} catch let error as CLIError {
    FileHandle.standardError.write(Data("herdr-desktop-launch: \(error)\n".utf8))
    exit(error.code)
} catch {
    FileHandle.standardError.write(Data("herdr-desktop-launch: unexpected error: \(error)\n".utf8))
    exit(70)
}
