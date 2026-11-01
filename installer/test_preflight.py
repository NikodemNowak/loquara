"""No-install guard tests. A harmless namesake process exits cooperatively via stdin."""
import argparse
import json
import os
import pathlib
import shutil
import subprocess
import tempfile

HARNESS = r'''
using System;
using System.Reflection;
using System.Security.Principal;
class GuardTests {
    static object Invoke(MethodInfo method, Func<int[]> list, Func<int,string> owner, string user) {
        return method.Invoke(null, new object[] { list, owner, user });
    }
    static void Expect(string wanted, object got) {
        if (got.ToString() != wanted) throw new Exception(wanted + " != " + got);
    }
    static int Main(string[] args) {
        if (args.Length == 1 && args[0] == "--hold") {
            Console.WriteLine("ready"); Console.ReadLine(); return 0;
        }
        var type = Assembly.LoadFrom(args[0]).GetType("RunningAppPreflight", true);
        if (args.Length == 3 && args[1] == "--owner") {
            var owner = type.GetMethod("OwnerSid", BindingFlags.NonPublic | BindingFlags.Static);
            var sid = (string)owner.Invoke(null, new object[] { Int32.Parse(args[2]) });
            using (var current = WindowsIdentity.GetCurrent()) {
                if (sid != current.User.Value) throw new Exception("Helper owner was not detected");
            }
            try {
                type.GetMethod("EnsureReady", BindingFlags.NonPublic | BindingFlags.Static).Invoke(null, null);
                throw new Exception("Running application was not blocked");
            } catch (TargetInvocationException blocked) {
                if (blocked.InnerException.GetType().Name != "RunningAppBlocked") throw;
            }
            Console.WriteLine("Real helper ownership and install guard passed; no installation invoked");
            return 0;
        }
        var check = type.GetMethod("CheckForUser", BindingFlags.NonPublic | BindingFlags.Static);
        Func<int,string> me = id => "current-user";
        Func<int[]> one = () => new[] { 1 };
        Expect("Ready", Invoke(check, () => new int[0], me, "current-user"));
        Expect("Running", Invoke(check, one, me, "current-user"));
        Expect("Ready", Invoke(check, one, id => "another-user", "current-user"));
        Expect("Unknown", Invoke(check, one, id => null, "current-user"));
        Expect("Unknown", Invoke(check, one, id => { throw new UnauthorizedAccessException(); }, "current-user"));
        Expect("Unknown", Invoke(check, () => { throw new Exception(); }, me, "current-user"));
        Expect("Unknown", Invoke(check, one, me, null));
        // A visible same-user process dominates an inaccessible different candidate.
        Expect("Running", Invoke(check, () => new[] { 1, 2 }, id => id == 1 ? null : "current-user", "current-user"));
        Console.WriteLine("8 guard classification checks passed");
        return 0;
    }
}
'''


def check(installer):
    result = subprocess.run([str(installer), '--check-running-app'], capture_output=True, text=True, timeout=10)
    assert result.returncode in (0, 4, 5), f'Preflight command missing or invalid: {result.returncode}'
    state = json.loads(result.stdout)
    assert state['installationStarted'] is False
    assert state['state'] in ('ready', 'running', 'unknown')
    return state


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--installer', required=True, type=pathlib.Path)
    args = parser.parse_args()
    baseline = check(args.installer)
    directory = pathlib.Path(tempfile.mkdtemp(prefix='Loquara-preflight-test-'))
    source = directory / 'GuardTests.cs'
    test_exe = directory / 'GuardTests.exe'
    helper_exe = directory / 'Loquara.exe'
    helper = None
    try:
        source.write_text(HARNESS, encoding='utf-8')
        csc = pathlib.Path(os.environ['WINDIR']) / 'Microsoft.NET/Framework64/v4.0.30319/csc.exe'
        subprocess.run([str(csc), '/nologo', '/target:exe', '/out:' + str(test_exe), str(source)], check=True, capture_output=True)
        subprocess.run([str(test_exe), str(args.installer)], check=True, timeout=10)
        shutil.copyfile(test_exe, helper_exe)
        helper = subprocess.Popen([str(helper_exe), '--hold'], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        assert helper.stdout.readline().strip() == 'ready'
        subprocess.run([str(test_exe), str(args.installer), '--owner', str(helper.pid)], check=True, timeout=10)
        observed = check(args.installer)
        assert observed['state'] == 'running', observed
        assert helper.poll() is None, 'Guard must never terminate even a harmless namesake process'
        helper.communicate('\n', timeout=10)
        assert helper.returncode == 0
        print(json.dumps({'baseline': baseline, 'sameUserProcess': observed, 'cooperativeHelperExit': True}))
    finally:
        if helper is not None and helper.poll() is None:
            helper.communicate('\n', timeout=10)
        for path in (source, test_exe, helper_exe):
            path.unlink(missing_ok=True)
        directory.rmdir()


if __name__ == '__main__':
    main()
