namespace CodeShellManager.Services;

/// <summary>
/// Splits a Windows-style commandline into (exe, args) using a simple
/// quote-aware single-pass scanner. The exe is unquoted; the args are returned
/// verbatim (including any internal quoting).
/// </summary>
public static class CommandLineSplitter
{
    public static (string exe, string args) Split(string commandLine)
    {
        if (string.IsNullOrWhiteSpace(commandLine)) return ("", "");
        var s = commandLine.TrimStart();

        if (s.StartsWith('"'))
        {
            int closing = s.IndexOf('"', 1);
            if (closing < 0) return (s.Trim('"'), "");
            string exe = s.Substring(1, closing - 1);
            string rest = s.Length > closing + 1 ? s[(closing + 1)..].TrimStart() : "";
            return (exe, rest);
        }

        int sp = s.IndexOf(' ');
        if (sp < 0) return (s, "");
        return (s[..sp], s[(sp + 1)..].TrimStart());
    }
}
