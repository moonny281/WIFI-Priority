using System;
using System.Diagnostics;
using System.IO;
using System.Security;
using System.Security.Principal;
using System.Text;

namespace WifiPriority.Services
{
    /// <summary>
    /// 开机自启：用“任务计划程序”创建登录触发、最高权限的任务。
    /// 好处：登录后自动以管理员权限启动且不弹 UAC；不受“电池供电不启动”等默认限制。
    /// 创建/删除任务需要管理员权限，会弹一次 UAC。
    /// </summary>
    public static class AutoStart
    {
        const string TaskName = "WifiPriority";

        static string ExePath => Environment.ProcessPath ?? Process.GetCurrentProcess().MainModule.FileName;

        public static bool IsEnabled()
        {
            try
            {
                var psi = new ProcessStartInfo("schtasks.exe")
                {
                    UseShellExecute = false,
                    CreateNoWindow = true,
                    RedirectStandardOutput = true,
                    RedirectStandardError = true
                };
                psi.ArgumentList.Add("/Query");
                psi.ArgumentList.Add("/TN");
                psi.ArgumentList.Add(TaskName);
                using (var p = Process.Start(psi))
                {
                    p.StandardOutput.ReadToEnd();
                    p.StandardError.ReadToEnd();
                    p.WaitForExit(8000);
                    return p.ExitCode == 0;
                }
            }
            catch { return false; }
        }

        public static bool Enable()
        {
            string tmp = Path.Combine(Path.GetTempPath(), "wifipriority_task.xml");
            try
            {
                File.WriteAllText(tmp, BuildXml(WindowsIdentity.GetCurrent().Name), Encoding.Unicode);
                return RunElevated($"/Create /TN \"{TaskName}\" /XML \"{tmp}\" /F");
            }
            catch { return false; }
            finally { try { File.Delete(tmp); } catch { } }
        }

        public static bool Disable()
        {
            return RunElevated($"/Delete /TN \"{TaskName}\" /F");
        }

        static bool RunElevated(string args)
        {
            try
            {
                var psi = new ProcessStartInfo("schtasks.exe", args)
                {
                    UseShellExecute = true,
                    Verb = "runas",
                    WindowStyle = ProcessWindowStyle.Hidden
                };
                using (var p = Process.Start(psi))
                {
                    p.WaitForExit();
                    return p.ExitCode == 0;
                }
            }
            catch { return false; }   // 用户取消了 UAC 等
        }

        static string BuildXml(string user)
        {
            string exe = SecurityElement.Escape(ExePath);
            string dir = SecurityElement.Escape(Path.GetDirectoryName(ExePath) ?? "");
            string u = SecurityElement.Escape(user);
            return
$@"<?xml version=""1.0"" encoding=""UTF-16""?>
<Task version=""1.4"" xmlns=""http://schemas.microsoft.com/windows/2004/02/mit/task"">
  <RegistrationInfo><Description>WiFi Priority auto start</Description></RegistrationInfo>
  <Triggers>
    <LogonTrigger><Enabled>true</Enabled><UserId>{u}</UserId><Delay>PT10S</Delay></LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id=""Author""><UserId>{u}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>HighestAvailable</RunLevel></Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>7</Priority>
  </Settings>
  <Actions Context=""Author"">
    <Exec><Command>{exe}</Command><Arguments>--background</Arguments><WorkingDirectory>{dir}</WorkingDirectory></Exec>
  </Actions>
</Task>";
        }
    }
}
