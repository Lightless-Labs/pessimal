//
//  SettingsNumber.swift
//  Pessimal — iOS client
//
//  Text fields into numbers, which is the only judgement the settings screen makes on its own.
//
//  Everything about whether a number is *acceptable* — an interval against the liveness policy, a
//  threshold's sign and magnitude, a dwell against the staleness budget — belongs to core and is
//  asked of `pollTuningValidated`, `draftAlertRule` and `validateAlertRule`. What is left is turning
//  the characters a keyboard produced into an `Int64` or a `Double`, and refusing to guess when they
//  are not one.
//

import Foundation

/// Parsing for the screen's numeric fields.
enum SettingsNumber {

    /// A whole number of seconds, or `nil` when the text is not one.
    ///
    /// Strict on purpose. The fields that use this are `.numberPad`, which offers digits and nothing
    /// else, so anything a grouping- or sign-tolerant parser would rescue arrived by paste — and a
    /// parser that reads `"1,000"` as `1` is how a ten-second poll interval becomes a one-second one
    /// without anybody being told.
    static func int64(from text: String) -> Int64? {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        return Int64(trimmed)
    }

    /// A threshold, or `nil` when the text is not a number.
    ///
    /// Accepts the **device locale's** decimal separator as well as a period, because that is what
    /// the `.decimalPad` keyboard offers: on a French device the only decimal key is a comma, and
    /// `Double("0,9")` is `nil`. Telling that user to "use a period for the decimal point" would be
    /// instructing them to press a key their keyboard does not have.
    ///
    /// `Locale.current` rather than the view's `\.locale` environment deliberately: the keyboard
    /// follows the system, so the system is the thing whose separator was typed.
    ///
    /// Grouping separators are **not** accepted. `"1,000"` is one thousand in one locale and one in
    /// another, and a field that silently picks is worse than a field that refuses.
    static func double(from text: String) -> Double? {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }

        if let value = Double(trimmed) { return value }

        guard let separator = Locale.current.decimalSeparator, separator != "." else { return nil }
        return Double(trimmed.replacingOccurrences(of: separator, with: "."))
    }
}
