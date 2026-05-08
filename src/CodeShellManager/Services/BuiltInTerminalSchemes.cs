using System.Collections.Generic;
using System.Text.Json;

namespace CodeShellManager.Services;

/// <summary>
/// Static lookup of color schemes that ship with Windows Terminal but are not
/// usually duplicated in a user's settings.json. Color values copied from the
/// Microsoft.Terminal repository defaults.
/// </summary>
public static class BuiltInTerminalSchemes
{
    private static readonly Dictionary<string, string> Schemes = new()
    {
        ["Campbell"] = """
            {"name":"Campbell","background":"#0C0C0C","foreground":"#CCCCCC","cursorColor":"#FFFFFF","selectionBackground":"#FFFFFF",
             "black":"#0C0C0C","red":"#C50F1F","green":"#13A10E","yellow":"#C19C00","blue":"#0037DA","purple":"#881798","cyan":"#3A96DD","white":"#CCCCCC",
             "brightBlack":"#767676","brightRed":"#E74856","brightGreen":"#16C60C","brightYellow":"#F9F1A5","brightBlue":"#3B78FF","brightPurple":"#B4009E","brightCyan":"#61D6D6","brightWhite":"#F2F2F2"}
            """,
        ["Campbell Powershell"] = """
            {"name":"Campbell Powershell","background":"#012456","foreground":"#CCCCCC","cursorColor":"#FFFFFF","selectionBackground":"#FFFFFF",
             "black":"#0C0C0C","red":"#C50F1F","green":"#13A10E","yellow":"#C19C00","blue":"#0037DA","purple":"#881798","cyan":"#3A96DD","white":"#CCCCCC",
             "brightBlack":"#767676","brightRed":"#E74856","brightGreen":"#16C60C","brightYellow":"#F9F1A5","brightBlue":"#3B78FF","brightPurple":"#B4009E","brightCyan":"#61D6D6","brightWhite":"#F2F2F2"}
            """,
        ["Vintage"] = """
            {"name":"Vintage","background":"#000000","foreground":"#C0C0C0","cursorColor":"#FFFFFF","selectionBackground":"#FFFFFF",
             "black":"#000000","red":"#800000","green":"#008000","yellow":"#808000","blue":"#000080","purple":"#800080","cyan":"#008080","white":"#C0C0C0",
             "brightBlack":"#808080","brightRed":"#FF0000","brightGreen":"#00FF00","brightYellow":"#FFFF00","brightBlue":"#0000FF","brightPurple":"#FF00FF","brightCyan":"#00FFFF","brightWhite":"#FFFFFF"}
            """,
        ["One Half Dark"] = """
            {"name":"One Half Dark","background":"#282C34","foreground":"#DCDFE4","cursorColor":"#FFFFFF","selectionBackground":"#FFFFFF",
             "black":"#282C34","red":"#E06C75","green":"#98C379","yellow":"#E5C07B","blue":"#61AFEF","purple":"#C678DD","cyan":"#56B6C2","white":"#DCDFE4",
             "brightBlack":"#5A6374","brightRed":"#E06C75","brightGreen":"#98C379","brightYellow":"#E5C07B","brightBlue":"#61AFEF","brightPurple":"#C678DD","brightCyan":"#56B6C2","brightWhite":"#DCDFE4"}
            """,
        ["One Half Light"] = """
            {"name":"One Half Light","background":"#FAFAFA","foreground":"#383A42","cursorColor":"#4F525D","selectionBackground":"#FFFFFF",
             "black":"#383A42","red":"#E45649","green":"#50A14F","yellow":"#C18301","blue":"#0184BC","purple":"#A626A4","cyan":"#0997B3","white":"#FAFAFA",
             "brightBlack":"#4F525D","brightRed":"#DF6C75","brightGreen":"#98C379","brightYellow":"#E4C07A","brightBlue":"#61AFEF","brightPurple":"#C577DD","brightCyan":"#56B5C1","brightWhite":"#FFFFFF"}
            """,
        ["Solarized Dark"] = """
            {"name":"Solarized Dark","background":"#002B36","foreground":"#839496","cursorColor":"#FFFFFF","selectionBackground":"#FFFFFF",
             "black":"#002B36","red":"#DC322F","green":"#859900","yellow":"#B58900","blue":"#268BD2","purple":"#D33682","cyan":"#2AA198","white":"#EEE8D5",
             "brightBlack":"#073642","brightRed":"#CB4B16","brightGreen":"#586E75","brightYellow":"#657B83","brightBlue":"#839496","brightPurple":"#6C71C4","brightCyan":"#93A1A1","brightWhite":"#FDF6E3"}
            """,
        ["Solarized Light"] = """
            {"name":"Solarized Light","background":"#FDF6E3","foreground":"#657B83","cursorColor":"#002B36","selectionBackground":"#FFFFFF",
             "black":"#002B36","red":"#DC322F","green":"#859900","yellow":"#B58900","blue":"#268BD2","purple":"#D33682","cyan":"#2AA198","white":"#EEE8D5",
             "brightBlack":"#073642","brightRed":"#CB4B16","brightGreen":"#586E75","brightYellow":"#657B83","brightBlue":"#839496","brightPurple":"#6C71C4","brightCyan":"#93A1A1","brightWhite":"#FDF6E3"}
            """,
        ["Tango Dark"] = """
            {"name":"Tango Dark","background":"#000000","foreground":"#D3D7CF","cursorColor":"#FFFFFF","selectionBackground":"#FFFFFF",
             "black":"#000000","red":"#CC0000","green":"#4E9A06","yellow":"#C4A000","blue":"#3465A4","purple":"#75507B","cyan":"#06989A","white":"#D3D7CF",
             "brightBlack":"#555753","brightRed":"#EF2929","brightGreen":"#8AE234","brightYellow":"#FCE94F","brightBlue":"#729FCF","brightPurple":"#AD7FA8","brightCyan":"#34E2E2","brightWhite":"#EEEEEC"}
            """,
        ["Tango Light"] = """
            {"name":"Tango Light","background":"#FFFFFF","foreground":"#555753","cursorColor":"#000000","selectionBackground":"#FFFFFF",
             "black":"#000000","red":"#CC0000","green":"#4E9A06","yellow":"#C4A000","blue":"#3465A4","purple":"#75507B","cyan":"#06989A","white":"#D3D7CF",
             "brightBlack":"#555753","brightRed":"#EF2929","brightGreen":"#8AE234","brightYellow":"#FCE94F","brightBlue":"#729FCF","brightPurple":"#AD7FA8","brightCyan":"#34E2E2","brightWhite":"#EEEEEC"}
            """,
    };

    public static JsonElement? Lookup(string? name)
    {
        if (string.IsNullOrEmpty(name)) return null;
        if (!Schemes.TryGetValue(name, out var json)) return null;
        return JsonDocument.Parse(json).RootElement.Clone();
    }
}
