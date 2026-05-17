using System;
using System.Collections.Generic;
using System.Linq;
using CodeShellManager.Models;
using CodeShellManager.Services;
using CodeShellManager.Terminal;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// Lifecycle tests for <see cref="SessionRunner"/> and <see cref="RunInstance"/> using a
/// hand-rolled <see cref="FakePseudoTerminal"/>. The PTY interface seam lives at
/// <see cref="IPseudoTerminal"/> — production code constructs a real ConPTY wrapper,
/// tests inject a fake that lets us simulate output bytes and exit codes synchronously.
/// </summary>
public class SessionRunnerTests
{
    private sealed class FakePseudoTerminal : IPseudoTerminal
    {
        public bool StartCalled { get; private set; }
        public bool DisposeCalled { get; private set; }
        public string? StartCommand { get; private set; }
        public string? StartArgs { get; private set; }
        public string? StartWorkingDirectory { get; private set; }

        public int? ExitCode { get; private set; }

        public event Action<string>? DataReceived;
        public event Action? Exited;

        public void Start(string command, string args, string workingDirectory,
            int cols = 220, int rows = 50, bool useJobObject = false)
        {
            StartCalled = true;
            StartCommand = command;
            StartArgs = args;
            StartWorkingDirectory = workingDirectory;
        }

        public void EmitData(string text) => DataReceived?.Invoke(text);

        public void EmitExit(int code)
        {
            ExitCode = code;
            Exited?.Invoke();
        }

        public void Dispose() => DisposeCalled = true;
    }

    private static ShellSession LocalSession() => new()
    {
        WorkingFolder = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile),
    };

    private static RunCommandItem Item(string id = "item-1", string label = "Build", string cmd = "dotnet build")
        => new() { Id = id, Label = label, CommandLine = cmd };

    // ── SessionRunner lifecycle ──────────────────────────────────────────────

    [Fact]
    public void Run_CreatesInstanceAndFiresInstancesChanged()
    {
        var fake = new FakePseudoTerminal();
        var runner = new SessionRunner(LocalSession(), () => fake);
        int fired = 0;
        runner.InstancesChanged += () => fired++;

        var inst = runner.Run(Item());

        Assert.NotNull(inst);
        Assert.True(fake.StartCalled);
        Assert.Single(runner.Instances);
        Assert.Same(inst, runner.Instances["item-1"]);
        Assert.True(fired >= 1, $"InstancesChanged should fire on Run; fired={fired}");
    }

    [Fact]
    public void Run_TwiceOnSameItem_DisposesPriorAndReplaces()
    {
        var fakes = new Queue<FakePseudoTerminal>(new[] { new FakePseudoTerminal(), new FakePseudoTerminal() });
        var runner = new SessionRunner(LocalSession(), () => fakes.Dequeue());

        var first = runner.Run(Item());
        var firstFake = (FakePseudoTerminal)GetPty(first)!;
        Assert.False(firstFake.DisposeCalled);

        var second = runner.Run(Item());
        var secondFake = (FakePseudoTerminal)GetPty(second)!;

        Assert.True(firstFake.DisposeCalled, "First instance's PTY should be disposed when re-Run replaces it.");
        Assert.False(secondFake.DisposeCalled);
        Assert.NotSame(first, second);
        Assert.Single(runner.Instances);
        Assert.Same(second, runner.Instances["item-1"]);
    }

    [Fact]
    public void Stop_DisposesInstanceAndFiresInstancesChanged()
    {
        var fake = new FakePseudoTerminal();
        var runner = new SessionRunner(LocalSession(), () => fake);
        runner.Run(Item());

        int firedAfterRun = 0;
        runner.InstancesChanged += () => firedAfterRun++;

        runner.Stop("item-1");

        Assert.True(fake.DisposeCalled);
        Assert.True(firedAfterRun >= 1, $"InstancesChanged should fire on Stop; fired={firedAfterRun}");
        // Stop keeps the instance around so the chip can still show failure state.
        Assert.Single(runner.Instances);
    }

    [Fact]
    public void Stop_UnknownItem_IsNoOp()
    {
        var runner = new SessionRunner(LocalSession(), () => new FakePseudoTerminal());
        int fired = 0;
        runner.InstancesChanged += () => fired++;

        runner.Stop("does-not-exist");

        Assert.Equal(0, fired);
        Assert.Empty(runner.Instances);
    }

    [Fact]
    public void Dismiss_RemovesInstanceFromDictionary()
    {
        var fake = new FakePseudoTerminal();
        var runner = new SessionRunner(LocalSession(), () => fake);
        runner.Run(Item());
        Assert.Single(runner.Instances);

        int fired = 0;
        runner.InstancesChanged += () => fired++;

        runner.Dismiss("item-1");

        Assert.Empty(runner.Instances);
        Assert.True(fake.DisposeCalled);
        Assert.True(fired >= 1, $"InstancesChanged should fire on Dismiss; fired={fired}");
    }

    [Fact]
    public void Dispose_StopsEveryRunningInstance()
    {
        var fakes = new List<FakePseudoTerminal>();
        var runner = new SessionRunner(LocalSession(), () =>
        {
            var f = new FakePseudoTerminal();
            fakes.Add(f);
            return f;
        });

        runner.Run(Item(id: "a", cmd: "echo a"));
        runner.Run(Item(id: "b", cmd: "echo b"));
        runner.Run(Item(id: "c", cmd: "echo c"));
        Assert.Equal(3, fakes.Count);

        runner.Dispose();

        Assert.All(fakes, f => Assert.True(f.DisposeCalled));
        Assert.Empty(runner.Instances);
    }

    [Fact]
    public void StopAll_StopsEveryRunningInstance()
    {
        var fakes = new List<FakePseudoTerminal>();
        var runner = new SessionRunner(LocalSession(), () =>
        {
            var f = new FakePseudoTerminal();
            fakes.Add(f);
            return f;
        });
        runner.Run(Item(id: "a"));
        runner.Run(Item(id: "b"));

        runner.StopAll();

        Assert.All(fakes, f => Assert.True(f.DisposeCalled));
        Assert.Empty(runner.Instances);
    }

    // ── RunInstance state transitions ───────────────────────────────────────

    [Fact]
    public void RunInstance_Start_TransitionsToRunning()
    {
        var fake = new FakePseudoTerminal();
        var inst = new RunInstance(Item(), () => fake);

        inst.Start(LocalSession());

        Assert.Equal(RunState.Running, inst.State);
        Assert.True(fake.StartCalled);
        Assert.NotNull(inst.StartedAt);
        Assert.Null(inst.EndedAt);
    }

    [Fact]
    public void RunInstance_ExitZero_TransitionsToExitedOk()
    {
        var fake = new FakePseudoTerminal();
        var inst = new RunInstance(Item(), () => fake);
        inst.Start(LocalSession());

        int stateChanges = 0;
        inst.StateChanged += () => stateChanges++;

        fake.EmitExit(0);

        Assert.Equal(RunState.ExitedOk, inst.State);
        Assert.Equal(0, inst.ExitCode);
        Assert.NotNull(inst.EndedAt);
        Assert.True(stateChanges >= 1);
    }

    [Fact]
    public void RunInstance_ExitNonZero_TransitionsToExitedFailed()
    {
        var fake = new FakePseudoTerminal();
        var inst = new RunInstance(Item(), () => fake);
        inst.Start(LocalSession());

        fake.EmitExit(1);

        Assert.Equal(RunState.ExitedFailed, inst.State);
        Assert.Equal(1, inst.ExitCode);
        Assert.NotNull(inst.EndedAt);
    }

    [Fact]
    public void RunInstance_ExitNegative_TransitionsToExitedFailed()
    {
        // Windows exit codes can be negative (e.g. STATUS_CONTROL_C_EXIT = unchecked((int)0xC000013A)).
        var fake = new FakePseudoTerminal();
        var inst = new RunInstance(Item(), () => fake);
        inst.Start(LocalSession());

        fake.EmitExit(unchecked((int)0xC000013A));

        Assert.Equal(RunState.ExitedFailed, inst.State);
    }

    [Fact]
    public void RunInstance_DisposeWhileRunning_ForcesExitedFailed()
    {
        var fake = new FakePseudoTerminal();
        var inst = new RunInstance(Item(), () => fake);
        inst.Start(LocalSession());
        Assert.Equal(RunState.Running, inst.State);

        inst.Dispose();

        Assert.True(fake.DisposeCalled);
        Assert.Equal(RunState.ExitedFailed, inst.State);
        Assert.NotNull(inst.EndedAt);
    }

    // ── Output buffer ────────────────────────────────────────────────────────

    [Fact]
    public void RunInstance_OutputBufferAppendsOnPtyData()
    {
        var fake = new FakePseudoTerminal();
        var inst = new RunInstance(Item(), () => fake);
        inst.Start(LocalSession());

        int outputEvents = 0;
        inst.OutputChanged += () => outputEvents++;

        fake.EmitData("hello ");
        fake.EmitData("world\n");

        Assert.Equal("hello world\n", inst.SnapshotOutput());
        Assert.True(outputEvents >= 2);
    }

    [Fact]
    public void RunInstance_OutputBufferStripsAnsiEscapeSequences()
    {
        var fake = new FakePseudoTerminal();
        var inst = new RunInstance(Item(), () => fake);
        inst.Start(LocalSession());

        // ESC[31m red ESC[0m → "red "
        fake.EmitData("\x1B[31mred\x1B[0m text");

        Assert.Equal("red text", inst.SnapshotOutput());
    }

    [Fact]
    public void RunInstance_OutputBufferCapsAtOneMegabyte()
    {
        var fake = new FakePseudoTerminal();
        var inst = new RunInstance(Item(), () => fake);
        inst.Start(LocalSession());

        // Push 1.5MB of plain ASCII (no ANSI, no \r — both get stripped).
        // The buffer is documented to cap at 1MB (1_000_000 chars) and drop from the head.
        const int chunkSize = 100_000;
        string chunk = new('x', chunkSize);
        for (int i = 0; i < 15; i++) fake.EmitData(chunk);

        string snap = inst.SnapshotOutput();
        Assert.Equal(1_000_000, snap.Length);
    }

    // ── Internal accessor for the private _pty field via reflection ──────────

    private static IPseudoTerminal? GetPty(RunInstance inst)
    {
        var field = typeof(RunInstance).GetField(
            "_pty",
            System.Reflection.BindingFlags.NonPublic | System.Reflection.BindingFlags.Instance);
        return (IPseudoTerminal?)field!.GetValue(inst);
    }
}
