using System.Linq;
using System.Runtime.InteropServices;

namespace CodeShellManager;

public partial class App : System.Windows.Application
{
    // When the WPF app is launched from a parent that hands us redirected std
    // handles (dotnet run, bash, cmd with redirection), ConPTY children inherit
    // those handles and bleed their output to the parent — bypassing the PTY
    // entirely and leaving xterm.js with no output. Detaching the console and
    // clearing std handles at startup prevents the leak.
    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool FreeConsole();

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool SetStdHandle(int nStdHandle, System.IntPtr handle);

    private const int STD_INPUT_HANDLE = -10;
    private const int STD_OUTPUT_HANDLE = -11;
    private const int STD_ERROR_HANDLE = -12;

    public static System.Windows.Forms.NotifyIcon? TrayIcon { get; private set; }

    /// <summary>
    /// When true (set by passing <c>--clean</c> on the command line), the app starts
    /// with no preloaded sessions and persists no state changes for this run. Useful
    /// for debugging: prior <c>state.json</c> contents are left untouched.
    /// </summary>
    public static bool CleanStart { get; private set; }

    public static string LogPath { get; } = System.IO.Path.Combine(
        System.Environment.GetFolderPath(System.Environment.SpecialFolder.ApplicationData),
        "CodeShellManager", "crash.log");

    protected override void OnStartup(System.Windows.StartupEventArgs e)
    {
        base.OnStartup(e);

        // Detach from any inherited parent console / std handles so ConPTY
        // children can't bleed their output back to the launching shell.
        FreeConsole();
        SetStdHandle(STD_INPUT_HANDLE,  System.IntPtr.Zero);
        SetStdHandle(STD_OUTPUT_HANDLE, System.IntPtr.Zero);
        SetStdHandle(STD_ERROR_HANDLE,  System.IntPtr.Zero);

        CleanStart = e.Args.Any(a =>
            string.Equals(a, "--clean", System.StringComparison.OrdinalIgnoreCase));

        // Catch unhandled exceptions and write to log
        DispatcherUnhandledException += (_, ex) =>
        {
            System.IO.Directory.CreateDirectory(System.IO.Path.GetDirectoryName(LogPath)!);
            System.IO.File.AppendAllText(LogPath,
                $"\n[{System.DateTime.Now:HH:mm:ss}] UNHANDLED: {ex.Exception}\n");
            ex.Handled = true; // keep app alive
        };
        AppDomain.CurrentDomain.UnhandledException += (_, ex) =>
        {
            System.IO.Directory.CreateDirectory(System.IO.Path.GetDirectoryName(LogPath)!);
            System.IO.File.AppendAllText(LogPath,
                $"\n[{System.DateTime.Now:HH:mm:ss}] FATAL: {ex.ExceptionObject}\n");
        };
        System.Threading.Tasks.TaskScheduler.UnobservedTaskException += (_, ex) =>
        {
            System.IO.Directory.CreateDirectory(System.IO.Path.GetDirectoryName(LogPath)!);
            System.IO.File.AppendAllText(LogPath,
                $"\n[{System.DateTime.Now:HH:mm:ss}] TASK: {ex.Exception}\n");
            ex.SetObserved();
        };

        TrayIcon = new System.Windows.Forms.NotifyIcon
        {
            Icon = CreateTrayIcon(),
            Visible = true,
            Text = "CodeShellManager"
        };

        TrayIcon.DoubleClick += (_, _) =>
        {
            MainWindow?.Show();
            MainWindow?.Activate();
        };
    }

    private static System.Drawing.Icon CreateTrayIcon()
    {
        try
        {
            var drawing = (System.Windows.Media.DrawingImage)Current.Resources["AppIconImage"];
            var visual = new System.Windows.Media.DrawingVisual();
            using (var ctx = visual.RenderOpen())
                ctx.DrawImage(drawing, new System.Windows.Rect(0, 0, 32, 32));

            var rtb = new System.Windows.Media.Imaging.RenderTargetBitmap(
                32, 32, 96, 96, System.Windows.Media.PixelFormats.Pbgra32);
            rtb.Render(visual);

            var encoder = new System.Windows.Media.Imaging.PngBitmapEncoder();
            encoder.Frames.Add(System.Windows.Media.Imaging.BitmapFrame.Create(rtb));

            using var ms = new System.IO.MemoryStream();
            encoder.Save(ms);
            ms.Seek(0, System.IO.SeekOrigin.Begin);
            using var bmp = new System.Drawing.Bitmap(ms);
            return System.Drawing.Icon.FromHandle(bmp.GetHicon());
        }
        catch
        {
            return System.Drawing.SystemIcons.Application;
        }
    }

    protected override void OnExit(System.Windows.ExitEventArgs e)
    {
        TrayIcon?.Dispose();
        base.OnExit(e);
    }
}
