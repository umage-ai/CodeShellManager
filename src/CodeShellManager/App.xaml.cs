namespace CodeShellManager;

public partial class App : System.Windows.Application
{
    public static System.Windows.Forms.NotifyIcon? TrayIcon { get; private set; }

    public static string LogPath { get; } = System.IO.Path.Combine(
        System.Environment.GetFolderPath(System.Environment.SpecialFolder.ApplicationData),
        "CodeShellManager", "crash.log");

    protected override void OnStartup(System.Windows.StartupEventArgs e)
    {
        base.OnStartup(e);

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
