using System;
using System.Collections.Generic;
using System.Linq;
using CodeShellManager.Models;

namespace CodeShellManager.Services;

public class SessionManager
{
    private readonly List<ShellSession> _sessions = [];
    private readonly List<SessionGroup> _groups = [];

    public IReadOnlyList<ShellSession> Sessions => _sessions;
    public IReadOnlyList<SessionGroup> Groups => _groups;

    public event Action<ShellSession>? SessionAdded;
    public event Action<ShellSession>? SessionRemoved;
    public event Action? SessionsChanged;

    public SessionManager()
    {
        _groups.Add(new SessionGroup { Name = "Default", SortOrder = 0 });
    }

    public ShellSession CreateSession(string name, string folder, string command, string args,
        string? groupId = null, string? colorOverride = null)
    {
        var session = new ShellSession
        {
            Name = string.IsNullOrWhiteSpace(name)
                ? System.IO.Path.GetFileName(folder.TrimEnd('/', '\\')) ?? command
                : name,
            WorkingFolder = folder,
            Command = command,
            Args = args,
            GroupId = groupId ?? _groups.FirstOrDefault()?.Id ?? "",
            ColorOverride = colorOverride,
            Status = SessionStatus.Running
        };

        _sessions.Add(session);
        SessionAdded?.Invoke(session);
        SessionsChanged?.Invoke();
        return session;
    }

    public void RemoveSession(string sessionId)
    {
        var session = _sessions.FirstOrDefault(s => s.Id == sessionId);
        if (session == null) return;
        _sessions.Remove(session);
        SessionRemoved?.Invoke(session);
        SessionsChanged?.Invoke();
    }

    public void RenameSession(string sessionId, string newName)
    {
        var session = _sessions.FirstOrDefault(s => s.Id == sessionId);
        if (session != null) session.Name = newName;
        SessionsChanged?.Invoke();
    }

    public void MoveSession(string sessionId, int newIndex)
    {
        var session = _sessions.FirstOrDefault(s => s.Id == sessionId);
        if (session == null) return;
        int cur = _sessions.IndexOf(session);
        newIndex = Math.Clamp(newIndex, 0, _sessions.Count - 1);
        if (cur == newIndex) return;
        _sessions.RemoveAt(cur);
        _sessions.Insert(newIndex, session);
        SessionsChanged?.Invoke();
    }

    public void UpdateStatus(string sessionId, SessionStatus status)
    {
        var session = _sessions.FirstOrDefault(s => s.Id == sessionId);
        if (session != null) session.Status = status;
        SessionsChanged?.Invoke();
    }

    public SessionGroup AddGroup(string name)
    {
        var group = new SessionGroup { Name = name, SortOrder = _groups.Count };
        _groups.Add(group);
        SessionsChanged?.Invoke();
        return group;
    }

    public void LoadFromState(AppState state)
    {
        _sessions.Clear();
        _groups.Clear();

        if (state.Groups.Count > 0)
            _groups.AddRange(state.Groups);
        else
            _groups.Add(new SessionGroup { Name = "Default" });

        // Sessions from state are configs only — they get relaunched fresh
        foreach (var s in state.Sessions)
            _sessions.Add(s);
    }

    public void PopulateState(AppState state)
    {
        state.Sessions = [.. _sessions];
        state.Groups = [.. _groups];
    }
}
