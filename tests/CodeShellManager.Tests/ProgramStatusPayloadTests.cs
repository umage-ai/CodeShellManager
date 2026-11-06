using System;
using System.Linq;
using System.Text;
using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// Tests for <see cref="ProgramStatusPayload"/> — the WPF-free parser behind OSC 7501
/// (Program Status Protocol). Every report is untrusted terminal output, and the spec says a
/// report that breaks any rule is dropped WHOLE, so most hostile cases assert null.
/// </summary>
public class ProgramStatusPayloadTests
{
    private static string B64(string s) => Convert.ToBase64String(Encoding.UTF8.GetBytes(s));

    // ---- query ----------------------------------------------------------------------------

    [Theory]
    [InlineData("?")]
    [InlineData(" ? ")]
    [InlineData("?:states=idle")]
    public void IsQuery_recognises_the_feature_query(string payload) =>
        Assert.True(ProgramStatusPayload.IsQuery(payload));

    [Theory]
    [InlineData("state=idle")]
    [InlineData("")]
    [InlineData(null)]
    public void IsQuery_rejects_reports(string? payload) =>
        Assert.False(ProgramStatusPayload.IsQuery(payload));

    [Fact]
    public void ProbeReply_is_OSC_7501_question_mark_with_ST()
    {
        Assert.Equal("\x1b]7501;?\x1b\\", ProgramStatusPayload.ProbeReply);
        Assert.Null(ProgramStatusPayload.TryParse("?"));    // a query is never a report
    }

    [Fact]
    public void ConPty_reply_is_the_probe_reply_as_win32_input_mode_key_events()
    {
        // Conhost drops plain OSC on the ConPTY input side; one key-down per character
        // (ESC [ Vk ; Sc ; Uc ; Kd ; Cs ; Rc _) is delivered to the program as those chars.
        string encoded = ProgramStatusPayload.ProbeReplyForConPty;
        var events = encoded.Split('_', StringSplitOptions.RemoveEmptyEntries);
        Assert.Equal(ProgramStatusPayload.ProbeReply.Length, events.Length);
        Assert.Equal("\x1b[0;0;27;1;0;1", events[0]);                 // ESC
        Assert.Equal("\x1b[0;0;93;1;0;1", events[1]);                 // ]
        var decoded = new string(events.Select(e => (char)int.Parse(e.Split(';')[2])).ToArray());
        Assert.Equal(ProgramStatusPayload.ProbeReply, decoded);
        Assert.DoesNotContain("\x1b]", encoded);                      // no raw OSC left to drop
    }

    // ---- states ---------------------------------------------------------------------------

    [Theory]
    [InlineData("idle", ProgramState.Idle)]
    [InlineData("working", ProgramState.Working)]
    [InlineData("done", ProgramState.Done)]
    [InlineData("blocked", ProgramState.Blocked)]
    [InlineData("error", ProgramState.Error)]
    [InlineData("clear", ProgramState.Clear)]
    public void Every_baseline_state_parses(string text, ProgramState expected) =>
        Assert.Equal(expected, ProgramStatusPayload.TryParse($"state={text}")!.State);

    [Theory]
    [InlineData("")]
    [InlineData("app=x")]
    [InlineData("state=sleeping")]
    [InlineData("state=Working")]          // values are case-sensitive
    [InlineData("state=")]
    public void Missing_or_unknown_state_ignores_the_report(string payload) =>
        Assert.Null(ProgramStatusPayload.TryParse(payload));

    [Fact]
    public void Claude_Code_shaped_report_parses()
    {
        // Exactly the field order Claude Code 2.1.319 emits.
        var r = ProgramStatusPayload.TryParse(
            $"state=blocked:app=claude-code:kind=permission:progress=40:msg={B64("Run npm test?")}");
        Assert.NotNull(r);
        Assert.Equal(ProgramState.Blocked, r!.State);
        Assert.Equal(BlockedKind.Permission, r.Kind);
        Assert.Equal(40, r.Progress);
        Assert.Equal("claude-code", r.App);
        Assert.Equal("Run npm test?", r.Message);
        Assert.Null(r.Id);
    }

    // ---- pair syntax ----------------------------------------------------------------------

    [Fact]
    public void Whitespace_around_pairs_is_trimmed() =>
        Assert.Equal(ProgramState.Working,
            ProgramStatusPayload.TryParse("  state = working :  progress=5 ")!.State);

    [Fact]
    public void Repeated_key_last_wins() =>
        Assert.Equal(ProgramState.Done,
            ProgramStatusPayload.TryParse("state=working:state=done")!.State);

    [Theory]
    [InlineData("state=working:garbage")]            // no '='
    [InlineData("state=working:=x")]                 // empty key
    [InlineData("state=working:Bad=1")]              // uppercase key
    [InlineData("state=working:k3y=1")]              // digit in key
    [InlineData("state=working:abcdefghijklmnopq=1")] // 17-byte key
    [InlineData("state=working:app=a b")]            // space inside a value
    [InlineData("state=working:app=a;b")]            // ';' not in alphabet
    public void Malformed_pairs_are_skipped_not_fatal(string payload)
    {
        var r = ProgramStatusPayload.TryParse(payload);
        Assert.NotNull(r);
        Assert.Equal(ProgramState.Working, r!.State);
        Assert.Null(r.App);
    }

    [Fact]
    public void Malformed_state_pair_means_no_state() =>
        Assert.Null(ProgramStatusPayload.TryParse("state=work ing"));

    [Fact]
    public void Unknown_keys_are_ignored() =>
        Assert.NotNull(ProgramStatusPayload.TryParse("state=idle:future=1:icon=x"));

    // ---- kind / progress ------------------------------------------------------------------

    [Theory]
    [InlineData("permission", BlockedKind.Permission)]
    [InlineData("question", BlockedKind.Question)]
    [InlineData("auth", BlockedKind.Auth)]
    [InlineData("other", BlockedKind.None)]
    public void Kind_parses_for_blocked(string kind, BlockedKind expected) =>
        Assert.Equal(expected, ProgramStatusPayload.TryParse($"state=blocked:kind={kind}")!.Kind);

    [Fact]
    public void Kind_is_ignored_outside_blocked() =>
        Assert.Equal(BlockedKind.None,
            ProgramStatusPayload.TryParse("state=working:kind=permission")!.Kind);

    [Theory]
    [InlineData("0", 0)]
    [InlineData("100", 100)]
    [InlineData("040", 40)]
    public void Progress_accepts_0_to_100(string text, int expected) =>
        Assert.Equal(expected, ProgramStatusPayload.TryParse($"state=working:progress={text}")!.Progress);

    [Theory]
    [InlineData("101")]
    [InlineData("-1")]
    [InlineData("4.5")]
    [InlineData("1000")]
    [InlineData("")]
    [InlineData("+5")]
    public void Invalid_progress_is_indeterminate_not_fatal(string text)
    {
        var r = ProgramStatusPayload.TryParse($"state=working:progress={text}");
        Assert.NotNull(r);
        Assert.Null(r!.Progress);
    }

    [Theory]
    [InlineData("idle")]
    [InlineData("done")]
    [InlineData("error")]
    public void Progress_is_ignored_outside_working_and_blocked(string state) =>
        Assert.Null(ProgramStatusPayload.TryParse($"state={state}:progress=50")!.Progress);

    [Fact]
    public void Blocked_carries_progress() =>
        Assert.Equal(70, ProgramStatusPayload.TryParse("state=blocked:progress=70")!.Progress);

    // ---- id / app -------------------------------------------------------------------------

    [Theory]
    [InlineData("build")]
    [InlineData("build/test/unit")]
    [InlineData("a/b/c/d/e/f/g/h")]                 // 8 levels
    public void Valid_ids_parse(string id) =>
        Assert.Equal(id, ProgramStatusPayload.TryParse($"state=working:id={id}")!.Id);

    [Theory]
    [InlineData("a/b/c/d/e/f/g/h/i")]               // 9 levels
    [InlineData("/build")]                          // empty segment
    [InlineData("build/")]
    [InlineData("a//b")]
    [InlineData("")]
    public void Invalid_id_ignores_the_report_rather_than_writing_root(string id) =>
        Assert.Null(ProgramStatusPayload.TryParse($"state=working:id={id}"));

    [Fact]
    public void Id_segment_over_32_bytes_ignores_the_report()
    {
        Assert.NotNull(ProgramStatusPayload.TryParse($"state=idle:id={new string('a', 32)}"));
        Assert.Null(ProgramStatusPayload.TryParse($"state=idle:id={new string('a', 33)}"));
    }

    [Fact]
    public void Id_over_128_bytes_ignores_the_report()
    {
        string ok = string.Join("/", Enumerable.Repeat(new string('a', 31), 4)); // 127
        string tooLong = ok + "aa";                                                // 129, last seg 33
        string tooLong2 = string.Join("/", Enumerable.Repeat(new string('a', 32), 4)) + "/a"; // 134
        Assert.NotNull(ProgramStatusPayload.TryParse($"state=idle:id={ok}"));
        Assert.Null(ProgramStatusPayload.TryParse($"state=idle:id={tooLong}"));
        Assert.Null(ProgramStatusPayload.TryParse($"state=idle:id={tooLong2}"));
    }

    [Fact]
    public void App_over_32_bytes_drops_the_report()
    {
        Assert.Equal(new string('x', 32),
            ProgramStatusPayload.TryParse($"state=idle:app={new string('x', 32)}")!.App);
        Assert.Null(ProgramStatusPayload.TryParse($"state=idle:app={new string('x', 33)}"));
    }

    // ---- base64 text ----------------------------------------------------------------------

    [Fact]
    public void Msg_and_title_decode_utf8()
    {
        var r = ProgramStatusPayload.TryParse($"state=done:title={B64("Café ✓")}:msg={B64("Tests passed – 42")}");
        Assert.Equal("Café ✓", r!.Title);
        Assert.Equal("Tests passed – 42", r.Message);
    }

    [Theory]
    [InlineData("SGk")]      // "Hi" unpadded
    [InlineData("SGk=")]     // padded
    public void Base64_padding_is_optional(string b64) =>
        Assert.Equal("Hi", ProgramStatusPayload.TryParse($"state=done:msg={b64}")!.Message);

    [Theory]
    [InlineData("S")]        // length % 4 == 1 can never be base64
    [InlineData("SGk==")]    // too much padding
    [InlineData("S=Gk")]     // padding in the middle
    [InlineData("SG-k")]     // url-safe alphabet is not standard base64
    [InlineData("SG_k")]
    public void Invalid_base64_drops_the_report(string b64) =>
        Assert.Null(ProgramStatusPayload.TryParse($"state=done:msg={b64}"));

    [Theory]
    [InlineData("line1\nline2")]
    [InlineData("carriage\rreturn")]
    [InlineData("esc \u001b[31m red")]
    [InlineData("bell\u0007")]
    [InlineData("del\u007f")]
    [InlineData("c1\u009b")]
    [InlineData("nul\0")]
    public void Control_characters_in_decoded_text_drop_the_report(string text)
    {
        Assert.Null(ProgramStatusPayload.TryParse($"state=done:msg={B64(text)}"));
        Assert.Null(ProgramStatusPayload.TryParse($"state=done:title={B64(text)}"));
    }

    [Fact]
    public void Invalid_utf8_drops_the_report()
    {
        string b64 = Convert.ToBase64String(new byte[] { 0x48, 0xff, 0xfe });
        Assert.Null(ProgramStatusPayload.TryParse($"state=done:msg={b64}"));
    }

    [Fact]
    public void Bidi_and_invisible_format_characters_are_neutralised()
    {
        // U+202E RIGHT-TO-LEFT OVERRIDE, U+200B ZERO WIDTH SPACE, U+2066 LRI.
        var r = ProgramStatusPayload.TryParse($"state=done:msg={B64("ok‮txt.exe​⁦")}");
        Assert.Equal("oktxt.exe", r!.Message);
    }

    [Fact]
    public void Title_decoded_limit_is_192_bytes()
    {
        Assert.NotNull(ProgramStatusPayload.TryParse($"state=idle:title={B64(new string('t', 192))}"));
        Assert.Null(ProgramStatusPayload.TryParse($"state=idle:title={B64(new string('t', 193))}"));
    }

    [Fact]
    public void Title_decoded_limit_counts_bytes_not_chars()
    {
        // 97 × "é" = 194 UTF-8 bytes, only 97 chars.
        Assert.Null(ProgramStatusPayload.TryParse($"state=idle:title={B64(new string('é', 97))}"));
    }

    [Fact]
    public void Msg_decoded_limit_is_2048_bytes()
    {
        Assert.NotNull(ProgramStatusPayload.TryParse($"state=idle:msg={B64(new string('m', 2048))}"));
        Assert.Null(ProgramStatusPayload.TryParse($"state=idle:msg={B64(new string('m', 2049))}"));
    }

    [Fact]
    public void Encoded_limits_are_enforced_before_decoding()
    {
        // 260 chars of valid-looking base64 for title (cap 256 encoded).
        Assert.Null(ProgramStatusPayload.TryParse($"state=idle:title={new string('A', 260)}"));
        Assert.Null(ProgramStatusPayload.TryParse($"state=idle:msg={new string('A', 2736)}"));
    }

    [Fact]
    public void Whole_sequence_over_4096_bytes_is_dropped()
    {
        // Padding with unknown keys keeps every individual field legal, so only the overall
        // cap can reject this.
        string filler = string.Concat(Enumerable.Repeat(":pad=xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx", 100));
        string payload = "state=working" + filler;
        Assert.True(payload.Length > 4096);
        Assert.Null(ProgramStatusPayload.TryParse(payload));

        string fits = "state=working" + filler[..(4096 - 9 - "state=working".Length)];
        Assert.NotNull(ProgramStatusPayload.TryParse(fits.TrimEnd(':')));
    }

    [Fact]
    public void A_bad_field_poisons_the_whole_report_even_with_a_valid_state()
    {
        // Nothing from a discarded report is applied — not even its state.
        Assert.Null(ProgramStatusPayload.TryParse($"state=blocked:kind=permission:msg={B64("a\nb")}"));
    }
}
