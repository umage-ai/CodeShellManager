using System;
using System.Collections.Generic;
using System.Globalization;
using System.Text;

namespace CodeShellManager.Services;

/// <summary>The <c>state</c> of an OSC 7501 report.</summary>
public enum ProgramState
{
    Idle,
    Working,
    Done,
    Blocked,
    Error,
    /// <summary>An action, not a state: removes the addressed record and its children.</summary>
    Clear,
}

/// <summary>The <c>kind</c> of a <see cref="ProgramState.Blocked"/> report.</summary>
public enum BlockedKind
{
    /// <summary>Not given, or given with a state other than blocked, or unrecognised.</summary>
    None,
    Permission,
    Question,
    Auth,
}

/// <summary>One validated OSC 7501 report. <see cref="Id"/> is null for the root record.</summary>
public sealed record ProgramStatusReport(
    ProgramState State,
    BlockedKind Kind = BlockedKind.None,
    int? Progress = null,
    string? App = null,
    string? Id = null,
    string? Title = null,
    string? Message = null);

/// <summary>
/// Parser and validator for the OSC 7501 Program Status Protocol
/// (https://www.superlogical.com/rex/docs/build/program-status). WPF-free so every rule is
/// unit-tested (<c>ProgramStatusPayloadTests</c>).
/// <para>
/// Wire form: <c>ESC ] 7501 ; key=value:key=value ST</c>. Everything here is untrusted
/// terminal output — a remote host, a <c>cat</c> of some file — so a report that breaks a
/// limit, carries invalid base64 or decodes to a control character is dropped WHOLE, as the
/// spec requires; nothing from it is applied.
/// </para>
/// </summary>
public static class ProgramStatusPayload
{
    // ── Limits, all from the spec ───────────────────────────────────────────────
    /// <summary>The whole escape sequence, introducer and terminator included.</summary>
    public const int MaxSequenceBytes = 4096;
    public const int MaxKeyBytes = 16;
    public const int MaxMsgEncodedBytes = 2732;
    public const int MaxMsgDecodedBytes = 2048;
    public const int MaxTitleEncodedBytes = 256;
    public const int MaxTitleDecodedBytes = 192;
    public const int MaxAppBytes = 32;
    public const int MaxIdBytes = 128;
    public const int MaxIdSegmentBytes = 32;
    public const int MaxIdDepth = 8;

    // "ESC ] 7501 ;" (7 bytes) + the longest ST, "ESC \" (2 bytes). The page hands us the
    // payload only, so the envelope is added back before comparing to MaxSequenceBytes.
    private const int EnvelopeBytes = 9;

    /// <summary>
    /// The terminal's answer to the feature query <c>OSC 7501 ; ? ST</c>. Fixed bytes, authored
    /// here — nothing from the query is echoed. Omitting <c>states=</c>/<c>kinds=</c> means
    /// "the baseline sets", which is exactly what we accept.
    /// <para>
    /// Claude Code sends the query at startup and, if it gets no reply before its
    /// device-attributes sentinel, logs <c>probe: no reply to OSC 7501 ; ?</c> and never reports.
    /// This raw form is what the bundled ConPTY delivers intact and in order; under the inbox
    /// ConPTY conhost answers that sentinel itself first, so there the reply is off by default
    /// (<see cref="ProgramStatusProbeGate"/>).
    /// </para>
    /// </summary>
    public const string ProbeReply = "\x1b]7501;?\x1b\\";

    /// <summary>
    /// <see cref="ProbeReply"/> as it must be WRITTEN to a Windows ConPTY: each character sent
    /// as a win32-input-mode key event (<c>ESC [ Vk ; Sc ; Uc ; Kd ; Cs ; Rc _</c>).
    /// <para>
    /// Measured against the inbox conhost (10.0.26100): its VT input parser silently DROPS
    /// every OSC sequence written to the pseudo console's input — the plain reply, BEL- or
    /// ST-terminated, never reaches the program (CSI and DCS replies do). Encoded as key
    /// events, the same bytes arrive at the program intact. See CLAUDE.md "Program status
    /// (OSC 7501)" for the remaining limitation (conhost answers DA1 itself, first).
    /// </para>
    /// </summary>
    public static readonly string ProbeReplyForConPty = EncodeAsWin32InputMode(ProbeReply);

    /// <summary>
    /// Encodes text as win32-input-mode key-down events, one per UTF-16 unit, which conhost
    /// turns back into the same characters for the program. Only ever applied to fixed,
    /// app-authored bytes.
    /// </summary>
    internal static string EncodeAsWin32InputMode(string text)
    {
        var sb = new StringBuilder(text.Length * 16);
        foreach (char c in text)
            sb.Append("\x1b[0;0;").Append((int)c).Append(";1;0;1_");
        return sb.ToString();
    }

    /// <summary>True when the OSC payload is the feature query (<c>?</c>) rather than a report.</summary>
    /// Only an exact <c>?</c> (after trimming) counts — the query carries no pairs.
    public static bool IsQuery(string? payload) =>
        payload != null && payload.Trim() == "?";

    /// <summary>
    /// Parses and validates one report payload (the text after <c>7501;</c>). Returns null when
    /// the report must be ignored: missing or unknown state, an invalid id, a limit exceeded,
    /// bad base64 or a control character in decoded text.
    /// </summary>
    public static ProgramStatusReport? TryParse(string? payload)
    {
        if (string.IsNullOrEmpty(payload)) return null;
        if (Encoding.UTF8.GetByteCount(payload) + EnvelopeBytes > MaxSequenceBytes) return null;

        var fields = new Dictionary<string, string>(StringComparer.Ordinal);
        foreach (var rawPair in payload.Split(':'))
        {
            string pair = rawPair.Trim();
            int eq = pair.IndexOf('=');
            if (eq <= 0) continue;                                   // malformed → skip
            string key = pair[..eq].Trim();
            string value = pair[(eq + 1)..].Trim();
            if (!IsKeyShaped(key)) continue;                         // malformed → skip
            if (key.Length > MaxKeyBytes) return null;               // a LIMIT → drop whole report
            if (!IsValidValue(value)) continue;                      // malformed → skip
            fields[key] = value;                                     // repeated → last wins
        }

        if (!fields.TryGetValue("state", out var stateText)) return null;
        ProgramState? state = stateText switch
        {
            "idle" => ProgramState.Idle,
            "working" => ProgramState.Working,
            "done" => ProgramState.Done,
            "blocked" => ProgramState.Blocked,
            "error" => ProgramState.Error,
            "clear" => ProgramState.Clear,
            _ => null,
        };
        if (state is null) return null;

        string? id = null;
        if (fields.TryGetValue("id", out var idText))
        {
            // An invalid id ignores the report rather than falling back to the root — writing
            // a malformed child report over the root would be a worse outcome than dropping it.
            if (!IsValidId(idText)) return null;
            id = idText;
        }

        string? app = null;
        // app: 1–32 of [A-Za-z0-9_.+-]; anything else is treated as absent (it is only a
        // grouping label, so an odd one is not worth dropping the state it came with).
        if (fields.TryGetValue("app", out var appText) && IsIdentifierSegment(appText, MaxAppBytes))
            app = appText;

        string? title = null;
        if (fields.TryGetValue("title", out var titleB64))
        {
            if (titleB64.Length > MaxTitleEncodedBytes) return null;
            if (!TryDecodeText(titleB64, MaxTitleDecodedBytes, out title)) return null;
        }

        string? msg = null;
        if (fields.TryGetValue("msg", out var msgB64))
        {
            if (msgB64.Length > MaxMsgEncodedBytes) return null;
            if (!TryDecodeText(msgB64, MaxMsgDecodedBytes, out msg)) return null;
        }

        BlockedKind kind = BlockedKind.None;
        if (state == ProgramState.Blocked && fields.TryGetValue("kind", out var kindText))
        {
            kind = kindText switch
            {
                "permission" => BlockedKind.Permission,
                "question" => BlockedKind.Question,
                "auth" => BlockedKind.Auth,
                _ => BlockedKind.None,
            };
        }

        int? progress = null;
        if (state is ProgramState.Working or ProgramState.Blocked
            && fields.TryGetValue("progress", out var progressText)
            && progressText.Length is > 0 and <= 3
            && int.TryParse(progressText, NumberStyles.None, CultureInfo.InvariantCulture, out int p)
            && p is >= 0 and <= 100)
        {
            // Out-of-range or non-integer progress is treated as absent (indeterminate). It is
            // not a size limit, so it does not justify dropping an otherwise valid report.
            progress = p;
        }

        return new ProgramStatusReport(state.Value, kind, progress, app, id,
            string.IsNullOrEmpty(title) ? null : title,
            string.IsNullOrEmpty(msg) ? null : msg);
    }

    /// <summary><c>[a-z]+</c>, any length — the 16-byte cap is a limit, checked separately.</summary>
    internal static bool IsKeyShaped(string key)
    {
        if (key.Length == 0) return false;
        foreach (char c in key)
            if (c is < 'a' or > 'z') return false;
        return true;
    }

    /// <summary><c>[A-Za-z0-9_.+-]{1,max}</c> — one id segment, or an <c>app</c> value.</summary>
    internal static bool IsIdentifierSegment(string s, int maxLength)
    {
        if (s.Length == 0 || s.Length > maxLength) return false;
        foreach (char c in s)
        {
            bool ok = c is >= 'A' and <= 'Z' or >= 'a' and <= 'z' or >= '0' and <= '9'
                      or '_' or '.' or '+' or '-';
            if (!ok) return false;
        }
        return true;
    }

    internal static bool IsValidValue(string value)
    {
        foreach (char c in value)
        {
            bool ok = c is >= 'A' and <= 'Z' or >= 'a' and <= 'z' or >= '0' and <= '9'
                      or '_' or '.' or ',' or '+' or '/' or '=' or '-';
            if (!ok) return false;
        }
        return true;
    }

    /// <summary>Slash path: 1–8 segments of <c>[A-Za-z0-9_.+-]{1,32}</c>, ≤128 bytes in all.</summary>
    internal static bool IsValidId(string id)
    {
        if (id.Length == 0 || id.Length > MaxIdBytes) return false;
        var segments = id.Split('/');
        if (segments.Length > MaxIdDepth) return false;
        foreach (var s in segments)
            if (!IsIdentifierSegment(s, MaxIdSegmentBytes)) return false;
        return true;
    }

    /// <summary>
    /// Standard base64 with optional padding → strict UTF-8, single line, no control
    /// characters (C0, DEL, C1). Unicode format characters — bidi overrides, zero-width
    /// joiners and the like — are removed rather than rejected: the spec asks terminals to
    /// neutralise them, and they are invisible, so stripping loses nothing a user could see.
    /// </summary>
    internal static bool TryDecodeText(string b64, int maxDecodedBytes, out string? text)
    {
        text = null;
        if (b64.Length == 0) { text = ""; return true; }
        if (b64.Contains('-') || b64.Contains('_') || b64.Contains(',') || b64.Contains('.'))
            return false;                                            // not standard base64

        string body = b64.TrimEnd('=');
        int padCount = b64.Length - body.Length;
        if (padCount > 2 || body.Contains('=')) return false;
        if (body.Length % 4 == 1) return false;
        string padded = body.Length % 4 == 0 ? body : body + new string('=', 4 - body.Length % 4);
        if (padCount > 0 && padded.Length != b64.Length) return false; // wrong amount of padding

        var bytes = new byte[padded.Length / 4 * 3];
        if (!Convert.TryFromBase64String(padded, bytes, out int written)) return false;
        if (written > maxDecodedBytes) return false;

        string decoded;
        try
        {
            decoded = new UTF8Encoding(false, throwOnInvalidBytes: true).GetString(bytes, 0, written);
        }
        catch (DecoderFallbackException)
        {
            return false;
        }

        // By rune, not by char: format characters outside the BMP (the tag block,
        // U+E0001 and U+E0020–U+E007F) are surrogate pairs and invisible all the same.
        var sb = new StringBuilder(decoded.Length);
        foreach (Rune r in decoded.EnumerateRunes())
        {
            if (Rune.IsControl(r)) return false;                     // C0, DEL, C1 — incl. CR/LF/NEL
            var cat = Rune.GetUnicodeCategory(r);
            // U+2028 / U+2029: "single line" means no line break of any kind.
            if (cat is UnicodeCategory.LineSeparator or UnicodeCategory.ParagraphSeparator) return false;
            if (cat == UnicodeCategory.Format) continue;
            sb.Append(r.ToString());
        }
        text = sb.ToString();
        return true;
    }
}
