using System;
using System.ComponentModel;
using System.Diagnostics;
using System.Globalization;
using System.IO;
using System.Reflection;
using System.Runtime.InteropServices;
using System.Security.AccessControl;
using System.Security.Cryptography;
using System.Security.Principal;
using System.Threading;
using System.Threading.Tasks;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Markup;
using System.Windows.Media;
using System.Windows.Media.Animation;
using Microsoft.Win32;
using ShapePath = System.Windows.Shapes.Path;

internal static class Program
{
    [STAThread]
    private static int Main(string[] args)
    {
        if (args.Length == 1 && args[0] == "--verify-payload")
        {
            try
            {
                using (var payload = Payload.Extract())
                    Console.WriteLine("{\"verified\":true,\"version\":\"" + PayloadMetadata.Version + "\",\"bytes\":" + PayloadMetadata.Length + ",\"sha256\":\"" + PayloadMetadata.Sha256 + "\",\"extracted\":true,\"installationStarted\":false}");
                return 0;
            }
            catch (Exception) { Console.WriteLine("{\"verified\":false,\"installationStarted\":false}"); return 1; }
        }
        if (args.Length == 1 && args[0] == "--check-running-app")
        {
            var state = RunningAppPreflight.Check();
            Console.WriteLine("{\"state\":\"" + state.ToString().ToLowerInvariant() + "\",\"installationStarted\":false}");
            return state == RunningAppState.Ready ? 0 : state == RunningAppState.Running ? 4 : 5;
        }
        if (args.Length != 0) return 2;
        bool owned;
        using (var mutex = new Mutex(true, "Local\\Loquara.Bootstrapper", out owned))
        {
            if (!owned) return 3;
            try { return new Application().Run(new InstallerWindow()); }
            finally { mutex.ReleaseMutex(); }
        }
    }
}

internal enum RunningAppState { Ready, Running, Unknown }

internal sealed class RunningAppBlocked : Exception
{
    internal readonly RunningAppState State;
    internal RunningAppBlocked(RunningAppState state) { State = state; }
}

internal static class RunningAppPreflight
{
    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern IntPtr OpenProcess(uint access, bool inheritHandle, int processId);
    [DllImport("advapi32.dll", SetLastError = true)]
    private static extern bool OpenProcessToken(IntPtr process, uint access, out IntPtr token);
    [DllImport("kernel32.dll")]
    private static extern bool CloseHandle(IntPtr handle);

    internal static RunningAppState Check()
    {
        try
        {
            using (var identity = WindowsIdentity.GetCurrent())
                return CheckForUser(FindCandidates, OwnerSid, identity.User == null ? null : identity.User.Value);
        }
        catch (Exception) { return RunningAppState.Unknown; }
    }

    internal static void EnsureReady()
    {
        var state = Check();
        if (state != RunningAppState.Ready) throw new RunningAppBlocked(state);
    }

    // The test seam exercises unknown owners and failed enumeration without touching real accounts.
    internal static RunningAppState CheckForUser(Func<int[]> enumerate, Func<int, string> owner, string currentSid)
    {
        if (String.IsNullOrWhiteSpace(currentSid)) return RunningAppState.Unknown;
        bool unknown = false;
        try
        {
            foreach (int id in enumerate())
            {
                try
                {
                    string sid = owner(id);
                    if (String.IsNullOrWhiteSpace(sid)) unknown = true;
                    else if (String.Equals(sid, currentSid, StringComparison.Ordinal)) return RunningAppState.Running;
                }
                catch (Exception) { unknown = true; }
            }
        }
        catch (Exception) { return RunningAppState.Unknown; }
        return unknown ? RunningAppState.Unknown : RunningAppState.Ready;
    }

    private static int[] FindCandidates()
    {
        var processes = Process.GetProcessesByName("loquara");
        try
        {
            var ids = new int[processes.Length];
            for (int i = 0; i < processes.Length; i++) ids[i] = processes[i].Id;
            return ids;
        }
        finally { foreach (var process in processes) process.Dispose(); }
    }

    private static string OwnerSid(int id)
    {
        // Query-limited access is enough; never request terminate, write, or debug access.
        IntPtr process = OpenProcess(0x1000, false, id);
        if (process == IntPtr.Zero) return null;
        IntPtr token = IntPtr.Zero;
        try
        {
            if (!OpenProcessToken(process, 0x0008, out token)) return null;
            using (var identity = new WindowsIdentity(token))
                return identity.User == null ? null : identity.User.Value;
        }
        finally { if (token != IntPtr.Zero) CloseHandle(token); CloseHandle(process); }
    }
}

// This directory and its ACL belong exclusively to this run. Cleanup never recurses.
internal sealed class Payload : IDisposable
{
    private readonly string directory;
    internal readonly string FileName;
    private Payload(string dir) { directory = dir; FileName = Path.Combine(dir, "Loquara-NSIS.exe"); }

    internal static Payload Extract()
    {
        string dir = Path.Combine(Path.GetTempPath(), "Loquara-setup-" + Guid.NewGuid().ToString("N"));
        var acl = new DirectorySecurity();
        acl.SetAccessRuleProtection(true, false);
        acl.AddAccessRule(new FileSystemAccessRule(WindowsIdentity.GetCurrent().User, FileSystemRights.FullControl,
            InheritanceFlags.ContainerInherit | InheritanceFlags.ObjectInherit, PropagationFlags.None, AccessControlType.Allow));
        Directory.CreateDirectory(dir, acl);
        var result = new Payload(dir);
        try
        {
            using (var input = Assembly.GetExecutingAssembly().GetManifestResourceStream("Loquara.Payload.exe"))
            using (var output = new FileStream(result.FileName, FileMode.CreateNew, FileAccess.Write, FileShare.None))
            {
                if (input == null || input.Length != PayloadMetadata.Length) throw new InvalidDataException();
                input.CopyTo(output);
            }
            using (var input = File.OpenRead(result.FileName))
            using (var sha = SHA256.Create())
            {
                string actual = BitConverter.ToString(sha.ComputeHash(input)).Replace("-", "").ToLowerInvariant();
                if (input.Length != PayloadMetadata.Length || actual != PayloadMetadata.Sha256) throw new InvalidDataException();
            }
            return result;
        }
        catch { result.Dispose(); throw; }
    }

    public void Dispose()
    {
        try
        {
            if (Path.GetDirectoryName(FileName) != directory ||
                Path.GetDirectoryName(directory) != Path.GetTempPath().TrimEnd(Path.DirectorySeparatorChar) ||
                !Path.GetFileName(directory).StartsWith("Loquara-setup-", StringComparison.Ordinal)) return;
            if (Directory.Exists(directory) && (File.GetAttributes(directory) & FileAttributes.ReparsePoint) != 0) return;
            if (File.Exists(FileName) && (File.GetAttributes(FileName) & FileAttributes.ReparsePoint) == 0) File.Delete(FileName);
            if (Directory.Exists(directory)) Directory.Delete(directory, false);
        }
        catch (IOException) { }
        catch (UnauthorizedAccessException) { }
    }
}

internal static class InstalledApp
{
    // Tauri NSIS writes this exact per-user key; never parse or execute UninstallString/DisplayIcon.
    internal static string Resolve()
    {
        foreach (var view in new[] { RegistryView.Registry64, RegistryView.Registry32 })
        {
            using (var hive = RegistryKey.OpenBaseKey(RegistryHive.CurrentUser, view))
            using (var key = hive.OpenSubKey(@"Software\Microsoft\Windows\CurrentVersion\Uninstall\Loquara"))
            {
                if (key == null || !MatchesVersion(key.GetValue("DisplayVersion") as string)) continue;
                if (!String.Equals(key.GetValue("MainBinaryName") as string, "loquara.exe", StringComparison.OrdinalIgnoreCase)) continue;
                string location = key.GetValue("InstallLocation") as string;
                if (String.IsNullOrWhiteSpace(location)) continue;
                location = location.Trim();
                if (location.Length > 1 && location[0] == '"' && location[location.Length - 1] == '"') location = location.Substring(1, location.Length - 2);
                if (!Path.IsPathRooted(location) || Path.GetPathRoot(location).Length != 3 ||
                    location.IndexOf('"') >= 0 || location.StartsWith(@"\\", StringComparison.Ordinal)) continue;
                string executable = Path.GetFullPath(Path.Combine(location, "loquara.exe"));
                if (!File.Exists(executable)) continue;
                var version = FileVersionInfo.GetVersionInfo(executable);
                if (MatchesVersion(version.ProductVersion)) return executable;
            }
        }
        throw new InvalidOperationException("installed-app-unverified");
    }

    private static bool MatchesVersion(string value)
    {
        Version actual, expected;
        return Version.TryParse(value, out actual) && Version.TryParse(PayloadMetadata.Version, out expected) &&
            actual.Major == expected.Major && actual.Minor == expected.Minor && actual.Build == expected.Build;
    }
}

internal sealed class InstallerWindow : Window
{
    private readonly bool polish = CultureInfo.CurrentUICulture.TwoLetterISOLanguageName == "pl";
    private readonly TextBlock heading;
    private readonly TextBlock description;
    private readonly TextBlock note;
    private readonly Button action;
    private readonly Button close;
    private readonly TranslateTransform left = new TranslateTransform();
    private readonly TranslateTransform right = new TranslateTransform();
    private readonly RotateTransform leftRotation = new RotateTransform(0, 16, 16);
    private readonly RotateTransform rightRotation = new RotateTransform(0, 16, 16);
    private bool working;
    private bool installed;
    private bool reducedMotion;

    private string T(string pl, string en) { return polish ? pl : en; }
    private static SolidColorBrush Brush(string color) { return (SolidColorBrush)new BrushConverter().ConvertFromString(color); }
    private static TextBlock Text(string value, double size, string color)
    {
        return new TextBlock { Text = value, FontSize = size, Foreground = Brush(color), TextWrapping = TextWrapping.Wrap };
    }

    internal InstallerWindow()
    {
        Title = T("Instalacja Loquara", "Install Loquara");
        Width = 440; Height = 508; ResizeMode = ResizeMode.NoResize; WindowStyle = WindowStyle.None;
        WindowStartupLocation = WindowStartupLocation.CenterScreen;
        Background = Brush("#151515"); Foreground = Brush("#f3f3f3"); FontFamily = new FontFamily(new Uri("pack://application:,,,/"), "./assets/#Inter");
        FontSize = 13; UseLayoutRounding = true; SnapsToDevicePixels = true;
        reducedMotion = !SystemParameters.ClientAreaAnimation;
        Resources.Add(typeof(Button), XamlReader.Parse(@"<Style xmlns='http://schemas.microsoft.com/winfx/2006/xaml/presentation' TargetType='Button'>
            <Setter Property='Cursor' Value='Hand'/><Setter Property='FontSize' Value='13'/><Setter Property='FontWeight' Value='SemiBold'/>
            <Setter Property='Template'><Setter.Value><ControlTemplate TargetType='Button'>
              <Border x:Name='Surface' xmlns:x='http://schemas.microsoft.com/winfx/2006/xaml' Background='{TemplateBinding Background}' CornerRadius='8' BorderThickness='1' BorderBrush='Transparent'>
                <ContentPresenter HorizontalAlignment='Center' VerticalAlignment='Center'/>
              </Border>
              <ControlTemplate.Triggers>
                <Trigger Property='IsMouseOver' Value='True'><Setter TargetName='Surface' Property='Opacity' Value='0.84'/></Trigger>
                <Trigger Property='IsPressed' Value='True'><Setter TargetName='Surface' Property='Opacity' Value='0.68'/></Trigger>
                <Trigger Property='IsKeyboardFocused' Value='True'><Setter TargetName='Surface' Property='BorderBrush' Value='#e5e5e5'/><Setter TargetName='Surface' Property='BorderThickness' Value='2'/></Trigger>
                <Trigger Property='IsEnabled' Value='False'><Setter TargetName='Surface' Property='Opacity' Value='0.5'/></Trigger>
              </ControlTemplate.Triggers>
            </ControlTemplate></Setter.Value></Setter></Style>"));
        var frame = new Border { BorderBrush = Brush("#333333"), BorderThickness = new Thickness(1) };
        var root = new Grid(); frame.Child = root; Content = frame;
        root.RowDefinitions.Add(new RowDefinition { Height = new GridLength(44) });
        root.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        root.RowDefinitions.Add(new RowDefinition { Height = new GridLength(42) });
        var header = new Grid { Background = Brush("#101010") };
        header.MouseLeftButtonDown += delegate(object sender, MouseButtonEventArgs e) { if (e.ButtonState == MouseButtonState.Pressed) DragMove(); };
        var brand = Text("Loquara", 13, "#f3f3f3"); brand.Margin = new Thickness(18, 0, 0, 0); brand.VerticalAlignment = VerticalAlignment.Center;
        header.Children.Add(brand);
        close = new Button { Width = 30, Height = 28, Margin = new Thickness(0, 0, 9, 0), HorizontalAlignment = HorizontalAlignment.Right, Background = Brushes.Transparent,
            ToolTip = T("Zamknij", "Close"), Content = new ShapePath { Data = Geometry.Parse("M0 0L10 10M10 0L0 10"), Stroke = Brush("#ababab"), StrokeThickness = 1.2, Width = 10, Height = 10 } };
        AutomationProperties.SetName(close, T("Zamknij instalator", "Close installer")); close.Click += delegate { Close(); }; header.Children.Add(close); root.Children.Add(header);
        var content = new Grid { Margin = new Thickness(52, 0, 52, 0) }; Grid.SetRow(content, 1); root.Children.Add(content);
        content.RowDefinitions.Add(new RowDefinition { Height = new GridLength(146) });
        content.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        content.RowDefinitions.Add(new RowDefinition { Height = new GridLength(44) });
        content.RowDefinitions.Add(new RowDefinition { Height = new GridLength(47) });
        var mark = new Grid { Width = 76, Height = 76, VerticalAlignment = VerticalAlignment.Center, Margin = new Thickness(0, 12, 0, 0) };
        var canvas = new Canvas { Width = 32, Height = 32 };
        // Exact Fold silhouette, sharing the web identity's two complementary turns.
        AddFold(canvas, "M5 17.5C5 9.8 9.5 5 17 5H27V11H17C13.4 11 11 13.3 11 17.5V21H5Z", left, leftRotation, "#f3f3f3");
        AddFold(canvas, "M27 14.5C27 22.2 22.5 27 15 27H5V21H15C18.6 21 21 18.7 21 14.5V11H27Z", right, rightRotation, "#e5e5e5");
        mark.Children.Add(new Viewbox { Child = canvas }); content.Children.Add(mark);
        var copy = new StackPanel { VerticalAlignment = VerticalAlignment.Top };
        heading = Text(T("Miejsce na Twoje słowa.", "A place for your words."), 26, "#f3f3f3"); heading.TextAlignment = TextAlignment.Center; heading.FontWeight = FontWeights.SemiBold; heading.LineHeight = 32;
        description = Text(T("Mów swobodnie. Loquara zamieni\nTwoje myśli w tekst.", "Speak freely. Loquara turns\nyour thoughts into text."), 14, "#ababab"); description.TextAlignment = TextAlignment.Center; description.LineHeight = 22; description.Margin = new Thickness(0, 14, 0, 0);
        copy.Children.Add(heading); copy.Children.Add(description); Grid.SetRow(copy, 1); content.Children.Add(copy);
        action = new Button { Content = T("Zainstaluj Loquara", "Install Loquara"), Background = Brush("#f3f3f3"), Foreground = Brush("#151515"), Height = 44, IsDefault = true };
        action.Click += ActionClicked; Grid.SetRow(action, 2); content.Children.Add(action);
        note = Text(T("Tylko dla Twojego konta Windows.", "For your Windows account only."), 11, "#ababab"); note.TextAlignment = TextAlignment.Center; note.Margin = new Thickness(0, 12, 0, 0); Grid.SetRow(note, 3); content.Children.Add(note);
        var footer = new Grid { Margin = new Thickness(18, 0, 18, 0) };
        var version = Text("Loquara " + PayloadMetadata.Version, 11, "#ababab"); version.VerticalAlignment = VerticalAlignment.Center; footer.Children.Add(version);
        var platform = Text("Windows", 11, "#ababab"); platform.HorizontalAlignment = HorizontalAlignment.Right; platform.VerticalAlignment = VerticalAlignment.Center; footer.Children.Add(platform);
        Grid.SetRow(footer, 2); root.Children.Add(footer);
        Loaded += delegate { Intro(); };
        Closing += OnClosing;
        SystemParameters.StaticPropertyChanged += SystemSettingsChanged;
        Closed += delegate { SystemParameters.StaticPropertyChanged -= SystemSettingsChanged; StopMotion(); };
    }

    private void AddFold(Canvas canvas, string geometry, TranslateTransform translation, RotateTransform rotation, string color)
    {
        var transforms = new TransformGroup(); transforms.Children.Add(rotation); transforms.Children.Add(translation);
        canvas.Children.Add(new ShapePath { Data = Geometry.Parse(geometry), Fill = Brush(color), RenderTransform = transforms });
    }
    private void Intro()
    {
        if (reducedMotion || working) return;
        Animate(left, TranslateTransform.XProperty, -3.2, 0, 700, false);
        Animate(right, TranslateTransform.XProperty, 3.2, 0, 700, false);
        Animate(left, TranslateTransform.YProperty, 3.2, 0, 700, false);
        Animate(right, TranslateTransform.YProperty, -3.2, 0, 700, false);
        Animate(leftRotation, RotateTransform.AngleProperty, -6, 0, 700, false);
        Animate(rightRotation, RotateTransform.AngleProperty, 6, 0, 700, false);
    }
    private static void Animate(Animatable target, DependencyProperty property, double from, double to, int milliseconds, bool loop)
    {
        var animation = new DoubleAnimation(from, to, TimeSpan.FromMilliseconds(milliseconds)) { AutoReverse = loop,
            EasingFunction = new ExponentialEase { Exponent = 5, EasingMode = EasingMode.EaseOut } };
        if (loop) animation.RepeatBehavior = RepeatBehavior.Forever;
        target.BeginAnimation(property, animation);
    }
    private void BusyMotion()
    {
        StopMotion(); if (reducedMotion) return;
        Animate(left, TranslateTransform.XProperty, 0, -1.4, 950, true);
        Animate(right, TranslateTransform.XProperty, 0, 1.4, 950, true);
    }
    private void StopMotion()
    {
        left.BeginAnimation(TranslateTransform.XProperty, null); right.BeginAnimation(TranslateTransform.XProperty, null);
        left.BeginAnimation(TranslateTransform.YProperty, null); right.BeginAnimation(TranslateTransform.YProperty, null);
        leftRotation.BeginAnimation(RotateTransform.AngleProperty, null); rightRotation.BeginAnimation(RotateTransform.AngleProperty, null);
    }
    private void SystemSettingsChanged(object sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName == "ClientAreaAnimation") Dispatcher.BeginInvoke(new Action(delegate { reducedMotion = !SystemParameters.ClientAreaAnimation; if (reducedMotion) StopMotion(); }));
    }
    private void OnClosing(object sender, CancelEventArgs e)
    {
        if (working) { e.Cancel = true; note.Text = T("Poczekaj na zakończenie instalacji.", "Please wait for installation to finish."); }
    }
    private async void ActionClicked(object sender, RoutedEventArgs e)
    {
        if (working) return;
        if (installed)
        {
            try
            {
                var path = InstalledApp.Resolve();
                Process.Start(new ProcessStartInfo(path) { UseShellExecute = false, WorkingDirectory = Path.GetDirectoryName(path) });
                Close();
            }
            catch (Exception) { description.Text = T("Nie udało się otworzyć aplikacji. Spróbuj ponownie lub uruchom Loquara z menu Start.", "Could not open the app. Try again or open Loquara from the Start menu."); }
            return;
        }
        working = true; action.IsEnabled = false; close.IsEnabled = false; StopMotion();
        heading.Text = T("Przygotowujemy Loquara.", "Getting Loquara ready.");
        description.Text = T("Sprawdzanie pakietu instalacyjnego…", "Checking the installation package…");
        action.Content = T("Instalowanie…", "Installing…"); note.Text = T("Nie uruchamiaj Loquara do zakończenia instalacji.", "Keep Loquara closed until installation finishes.");
        try
        {
            await Task.Run(delegate
            {
                RunningAppPreflight.EnsureReady();
                using (var payload = Payload.Extract())
                using (var process = new Process())
                {
                    process.StartInfo = new ProcessStartInfo(payload.FileName, "/S") { UseShellExecute = false, WorkingDirectory = Path.GetDirectoryName(payload.FileName) };
                    // Recheck after extraction, immediately before NSIS. The stock payload has
                    // no atomic non-terminating handoff: a newly started app can still race this check.
                    RunningAppPreflight.EnsureReady();
                    if (!process.Start()) throw new InvalidOperationException();
                    Dispatcher.Invoke(new Action(delegate { description.Text = T("Instalacja aplikacji i wymaganych składników.\nTo może potrwać chwilę.", "Installing the app and required components.\nThis may take a moment."); BusyMotion(); }));
                    process.WaitForExit();
                    Dispatcher.Invoke(new Action(StopMotion));
                    if (process.ExitCode != 0) throw new InstallFailure(process.ExitCode);
                    InstalledApp.Resolve();
                }
            });
            installed = true;
            heading.Text = T("Możesz już mówić.", "Ready for your words.");
            description.Text = T("Loquara jest zainstalowana.\nOtwórz aplikację i zacznij od pierwszej myśli.", "Loquara is installed.\nOpen the app and start with your first thought.");
            action.Content = T("Otwórz Loquara", "Open Loquara");
            note.Text = T("Aplikacja otworzy się, gdy zechcesz.", "Open the app when you are ready.");
        }
        catch (Exception error)
        {
            heading.Text = T("Spróbujmy ponownie.", "Let’s try again.");
            var installError = error as InstallFailure;
            var blocked = error as RunningAppBlocked;
            if (blocked != null)
            {
                heading.Text = T("Najpierw zamknij Loquara.", "Close Loquara first.");
                description.Text = blocked.State == RunningAppState.Running
                    ? T("Dokończ dyktowanie i zamknij Loquara, także z zasobnika systemowego. Potem spróbuj ponownie.", "Finish your dictation and quit Loquara, including from the system tray. Then try again.")
                    : T("Nie można sprawdzić, czy Loquara jest uruchomiona. Zamknij aplikację i spróbuj ponownie. Instalacja nie została rozpoczęta.", "Could not check whether Loquara is running. Close the app and try again. Installation has not started.");
            }
            else if (installError != null)
                description.Text = T("Instalacja nie została ukończona (kod ", "Installation did not finish (code ") + installError.Code + T("). Zamknij Loquara, sprawdź połączenie i spróbuj ponownie.", "). Close Loquara, check your connection and try again.");
            else if (error is InvalidDataException)
                description.Text = T("Pakiet instalacyjny jest uszkodzony. Pobierz instalator ponownie.", "The installation package is damaged. Download the installer again.");
            else if (error is InvalidOperationException)
                description.Text = T("Nie można potwierdzić instalacji tej wersji. Zamknij Loquara i spróbuj ponownie.", "This version could not be verified after installation. Close Loquara and try again.");
            else description.Text = T("Nie udało się zainstalować aplikacji. Sprawdź wolne miejsce i uprawnienia konta, a następnie spróbuj ponownie.", "The app could not be installed. Check free disk space and account permissions, then try again.");
            action.Content = T("Spróbuj ponownie", "Try again");
            note.Text = blocked != null ? T("Instalacja nie została rozpoczęta.", "Installation has not started.") : T("Możesz bezpiecznie zamknąć instalator.", "You can safely close this installer.");
        }
        finally { StopMotion(); working = false; action.IsEnabled = true; close.IsEnabled = true; }
    }
    private sealed class InstallFailure : Exception
    {
        internal readonly int Code;
        internal InstallFailure(int code) { Code = code; }
    }
}
