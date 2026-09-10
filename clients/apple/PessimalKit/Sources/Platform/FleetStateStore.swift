import Foundation

/// What a read of the cached fleet state found.
///
/// Three cases, not `String?`, because the two ways of having nothing are different facts. A file
/// that is not there is a first launch. A file that is there and could not be read is a defect —
/// a permissions change, a full disk, a truncated write — and folding it into `nil` would hide the
/// one of the two that somebody should look at. The core makes the same distinction one level in:
/// `RestoreReportRecord` separates `discardedUnreadable` from `discardedIncompatible` for exactly
/// this reason, and it would be odd for the layer above to be less careful than the layer below.
public enum FleetStateLoad: Sendable, Equatable {
    /// No cache has been written yet, or it was cleared. A cold, blind start is correct.
    case absent
    /// The bytes were read. Whether they are *valid* is not this layer's judgement — see the note
    /// on ``FleetStateStore/load()``.
    case restored(String)
    /// A cache exists but this process could not read it. The reason is for a log or a diagnostics
    /// pane; the app should carry on as though the cache were absent.
    case unreadable(reason: String)
}

/// Persistence for the opaque state string `FleetSession.exportState()` produces.
///
/// The point is a relaunch that is not blind: without it, every launch shows an empty fleet until
/// the first poll returns, and a menu bar app that is opened, glanced at and dismissed may be
/// closed again before that happens.
///
/// The store handles a string. It does not know it is JSON, does not know what is in it, and must
/// not learn — the format is the core's, the schema version inside it is the core's, and the
/// decision about what to salvage from an old one is `FleetSession.restoreReport()`.
public protocol FleetStateStore: Sendable {
    /// Reads the cache.
    ///
    /// Never throws and never crashes: this sits on the launch path, and an app that refuses to
    /// start because its cache is a schema behind is worse than one that starts empty.
    ///
    /// Note what is *not* checked. The bytes are not parsed, the JSON is not validated, the schema
    /// version is not inspected. `.restored` means "these bytes were on disk and are UTF-8", not
    /// "this is a good cache". Hand the string to `FleetSession.init(…, cachedStateJson:)` and read
    /// `restoreReport()`: the core accepts a bad cache without erroring and reports what it
    /// salvaged, and a validation pass here would be a second opinion that can only disagree.
    func load() -> FleetStateLoad

    /// Writes the cache, replacing any previous one.
    ///
    /// Throws rather than swallowing, because a save failure is not on the launch path and is worth
    /// surfacing — a cache that silently stops being written looks exactly like one that works.
    ///
    /// Does blocking file I/O. Call it off the main actor, and call it at a lull: after a poll
    /// settles and when the app is about to terminate, not on every UI tick.
    func save(_ stateJSON: String) throws

    /// Deletes the cache. Succeeds when there is nothing to delete.
    ///
    /// **The Model layer must call this when the backend base URL or the environment changes.** The
    /// state string carries a fleet, not the address it was observed at, so nothing downstream can
    /// tell that the hosts in it belong to a different environment: `FleetSession` will restore
    /// staging's hosts under production's name, report a clean `restoreReport()`, and show a fleet
    /// that was never there until the first poll replaces it. There is no fix inside this file —
    /// stamping the URL and environment into a sidecar would be this layer modelling identity the
    /// core owns — so it is a call the layer that changes those settings has to make.
    func clear() throws
}

/// The real store: one file under Application Support.
///
/// `~/Library/Application Support/<bundle identifier>/fleet-state.json` for an unsandboxed build,
/// and the same path inside the app's container once the target enables App Sandbox — `FileManager`
/// redirects it, silently and correctly. The one consequence of that redirect worth knowing: a
/// cache written by today's unsandboxed development build is not the file tomorrow's sandboxed
/// build reads. That surfaces as one cold start, which is exactly what ``FleetStateLoad/absent`` is
/// for.
public final class FileFleetStateStore: FleetStateStore, @unchecked Sendable {
    // `@unchecked` because `FileManager` is not annotated `Sendable` in every SDK this may be built
    // against. `.default` is documented thread-safe for the operations used here, and the URL beside
    // it is a `let`.
    private let fileManager: FileManager

    /// Where the cache lives. Public so a diagnostics pane can show it, or reveal it in Finder.
    public let fileURL: URL

    /// The directory name under Application Support.
    ///
    /// A literal, not `Bundle.main.bundleIdentifier`, for the same reason ``KeychainAPIKeyStore``
    /// uses a literal service: the bundle identifier is `nil` in a test harness and different again
    /// under a preview agent, and a cache whose path depends on how the process was launched is a
    /// cache that is never found twice.
    public static let defaultDirectoryName = "com.lightless-labs.pessimal.macos"

    /// The default location: `<Application Support>/<directoryName>/<fileName>`.
    ///
    /// - Parameters:
    ///   - fileManager: `.default` outside tests.
    ///   - directoryName: Overridden by tests so a run cannot touch a real install's cache.
    ///   - fileName: As above.
    public convenience init(
        fileManager: FileManager = .default,
        directoryName: String = FileFleetStateStore.defaultDirectoryName,
        fileName: String = "fleet-state.json"
    ) {
        // Resolution is non-throwing on purpose: this runs during app construction, and there is no
        // useful behaviour for "the app could not work out where Application Support is" other than
        // to carry on and let the first `save` report it. `urls(for:in:)` answers from a table and
        // does not touch the disk; the home-relative fallback is what it would have produced anyway.
        let base = fileManager.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? URL(fileURLWithPath: NSHomeDirectory(), isDirectory: true)
                .appending(path: "Library/Application Support", directoryHint: .isDirectory)

        self.init(
            fileURL: base
                .appending(path: directoryName, directoryHint: .isDirectory)
                .appending(path: fileName, directoryHint: .notDirectory),
            fileManager: fileManager
        )
    }

    /// An explicit location, for tests that want a temporary directory.
    public init(fileURL: URL, fileManager: FileManager = .default) {
        self.fileURL = fileURL
        self.fileManager = fileManager
    }

    public func load() -> FleetStateLoad {
        do {
            let data = try Data(contentsOf: fileURL)
            guard let stateJSON = String(data: data, encoding: .utf8) else {
                // Not `.absent`. Bytes that are not UTF-8 were written by something other than this
                // app, or by a write that was cut short, and either is worth knowing about.
                return .unreadable(reason: "the cache is not valid UTF-8")
            }
            return .restored(stateJSON)
        } catch let error as CocoaError where error.code == .fileNoSuchFile || error.code == .fileReadNoSuchFile {
            // A first launch, or a `clear()`. The directory not existing lands here too.
            return .absent
        } catch {
            return .unreadable(reason: error.localizedDescription)
        }
    }

    public func save(_ stateJSON: String) throws {
        try fileManager.createDirectory(
            at: fileURL.deletingLastPathComponent(),
            withIntermediateDirectories: true,
            attributes: [.posixPermissions: 0o700]
        )

        // `.atomic` writes a temporary file and renames it over the destination, so a crash or a
        // full disk mid-write leaves the previous cache intact rather than a truncated one. A
        // truncated cache is not fatal — the core reports `discardedUnreadable` — but a launch that
        // silently loses a good fleet to a save that was interrupted is avoidable, and this is how.
        try Data(stateJSON.utf8).write(to: fileURL, options: [.atomic])

        // The rename above brings the temporary file's permissions with it, which are the process
        // umask's and typically world-readable. The cache holds a host inventory and its metrics:
        // not credentials, but not other users' business either. Tightening it is best-effort — the
        // data is already safely written, and failing the save over a `chmod` would discard a good
        // cache to protect it.
        try? fileManager.setAttributes([.posixPermissions: 0o600], ofItemAtPath: fileURL.path)
    }

    public func clear() throws {
        do {
            try fileManager.removeItem(at: fileURL)
        } catch let error as CocoaError where error.code == .fileNoSuchFile || error.code == .fileReadNoSuchFile {
            // Already gone is the outcome asked for.
            return
        }
    }
}

/// A store that keeps the state in memory, for tests and SwiftUI previews.
public final class InMemoryFleetStateStore: FleetStateStore, @unchecked Sendable {
    // `@unchecked`: the two values below are guarded by this lock.
    private let lock = NSLock()
    private var stored: FleetStateLoad
    private var saveFailure: (any Error)?

    /// - Parameter initial: What ``load()`` answers before anything is saved. Defaults to a cold
    ///   start; pass `.unreadable(reason:)` to drive the branch a corrupt file would take.
    public init(initial: FleetStateLoad = .absent) {
        stored = initial
    }

    /// Makes every subsequent ``save(_:)`` throw, so the failure branch can be driven.
    public func failEverySave(with error: any Error) {
        lock.withLock { saveFailure = error }
    }

    /// Undoes ``failEverySave(with:)``.
    public func stopFailing() {
        lock.withLock { saveFailure = nil }
    }

    public func load() -> FleetStateLoad {
        lock.withLock { stored }
    }

    public func save(_ stateJSON: String) throws {
        try lock.withLock {
            if let saveFailure { throw saveFailure }
            stored = .restored(stateJSON)
        }
    }

    public func clear() throws {
        lock.withLock { stored = .absent }
    }
}
