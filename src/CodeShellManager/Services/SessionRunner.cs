using System;
using System.Collections.Generic;
using System.Linq;
using CodeShellManager.Models;

namespace CodeShellManager.Services;

/// <summary>
/// Owns the per-item run state for one session. One <see cref="RunInstance"/>
/// per <see cref="RunCommandItem.Id"/>; running an item again disposes the prior
/// instance and creates a fresh one (kill-and-restart semantics).
/// </summary>
public class SessionRunner : IDisposable
{
    private readonly ShellSession _session;
    private readonly Dictionary<string, RunInstance> _instances = new();

    /// <summary>Fires when any instance is added, replaced, or removed, or any state changes.</summary>
    public event Action? InstancesChanged;

    public SessionRunner(ShellSession session) { _session = session; }

    public IReadOnlyDictionary<string, RunInstance> Instances => _instances;

    public RunInstance? GetInstance(string itemId) =>
        _instances.TryGetValue(itemId, out var inst) ? inst : null;

    /// <summary>
    /// Starts (or restarts) a run for the given item. If a prior instance exists,
    /// it is disposed first (which kills the child process tree).
    /// </summary>
    public RunInstance Run(RunCommandItem item)
    {
        if (_instances.TryGetValue(item.Id, out var existing))
        {
            existing.StateChanged -= OnInstanceStateChanged;
            existing.OutputChanged -= OnInstanceOutputChanged;
            existing.Dispose();
            _instances.Remove(item.Id);
        }

        var inst = new RunInstance(item);
        inst.StateChanged += OnInstanceStateChanged;
        inst.OutputChanged += OnInstanceOutputChanged;
        _instances[item.Id] = inst;
        inst.Start(_session);
        InstancesChanged?.Invoke();
        return inst;
    }

    /// <summary>
    /// Stops (kills) the run for the given item. The instance is kept around so
    /// the chip still shows the failed/cancelled state — call <see cref="Dismiss"/>
    /// to remove it entirely.
    /// </summary>
    public void Stop(string itemId)
    {
        if (_instances.TryGetValue(itemId, out var inst))
        {
            inst.Stop();
            InstancesChanged?.Invoke();
        }
    }

    /// <summary>
    /// Removes the instance entirely (kills if still running, then forgets it).
    /// The chip disappears; next click on the item starts fresh.
    /// </summary>
    public void Dismiss(string itemId)
    {
        if (_instances.TryGetValue(itemId, out var inst))
        {
            inst.StateChanged -= OnInstanceStateChanged;
            inst.OutputChanged -= OnInstanceOutputChanged;
            inst.Dispose();
            _instances.Remove(itemId);
            InstancesChanged?.Invoke();
        }
    }

    /// <summary>Kills every running child. Called on parent session close / sleep / app exit.</summary>
    public void StopAll()
    {
        foreach (var inst in _instances.Values.ToList())
        {
            inst.StateChanged -= OnInstanceStateChanged;
            inst.OutputChanged -= OnInstanceOutputChanged;
            inst.Dispose();
        }
        _instances.Clear();
        InstancesChanged?.Invoke();
    }

    private void OnInstanceStateChanged() => InstancesChanged?.Invoke();
    private void OnInstanceOutputChanged() => InstancesChanged?.Invoke();

    public void Dispose() => StopAll();
}
