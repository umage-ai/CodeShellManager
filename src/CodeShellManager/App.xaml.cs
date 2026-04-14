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
            Icon = System.Drawing.SystemIcons.Application,
            Visible = true,
            Text = "CodeShellManager"
        };

        TrayIcon.DoubleClick += (_, _) =>
        {
            MainWindow?.Show();
            MainWindow?.Activate();
        };
    }

    protected override void OnExit(System.Windows.ExitEventArgs e)
    {
        TrayIcon?.Dispose();
        base.OnExit(e);
    }
}
