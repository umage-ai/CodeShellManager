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
    public event Action? GroupsChanged;

    public ShellSession CreateSession(string name, string folder, string command, string args,
        string? groupId = null, string? colorOverride = null, string? afterSessionId = null)
    {
        var session = new ShellSession
        {
            Name = string.IsNullOrWhiteSpace(name)
                ? System.IO.Path.GetFileName(folder.TrimEnd('/', '\\')) ?? command
                : name,
            WorkingFolder = folder,
            Command = command,
            Args = args,
            GroupId = groupId ?? "",
            ColorOverride = colorOverride,
            Status = SessionStatus.Running
        };

        int insertAt = -1;
        if (!string.IsNullOrEmpty(afterSessionId))
        {
            int parentIdx = _sessions.FindIndex(s => s.Id == afterSessionId);
            if (parentIdx >= 0) insertAt = parentIdx + 1;
        }
        if (insertAt >= 0 && insertAt <= _sessions.Count) _sessions.Insert(insertAt, session);
        else _sessions.Add(session);

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
        GroupsChanged?.Invoke();
        return group;
    }

    public void RenameGroup(string groupId, string newName)
    {
        var group = _groups.FirstOrDefault(g => g.Id == groupId);
        if (group == null || string.IsNullOrWhiteSpace(newName)) return;
        group.Name = newName.Trim();
        GroupsChanged?.Invoke();
    }

    /// <summary>
    /// Removes a group. Any sessions assigned to it are moved to "ungrouped"
    /// (GroupId cleared). Sessions themselves are not deleted.
    /// </summary>
    public void RemoveGroup(string groupId)
    {
        var group = _groups.FirstOrDefault(g => g.Id == groupId);
        if (group == null) return;
        foreach (var s in _sessions)
        {
            if (s.GroupId == groupId) s.GroupId = "";
        }
        _groups.Remove(group);
        GroupsChanged?.Invoke();
        SessionsChanged?.Invoke();
    }

    /// <summary>Assigns a session to a group (empty/null groupId = ungrouped).</summary>
    public void SetSessionGroup(string sessionId, string? groupId)
    {
        var session = _sessions.FirstOrDefault(s => s.Id == sessionId);
        if (session == null) return;
        session.GroupId = groupId ?? "";
        SessionsChanged?.Invoke();
    }

    public void LoadFromState(AppState state)
    {
        _sessions.Clear();
        _groups.Clear();
        _groups.AddRange(state.Groups);

        // Sessions from state are configs only — they get relaunched fresh
        foreach (var s in state.Sessions)
            _sessions.Add(s);

        // Legacy migration: previous versions auto-created a single "Default" group
        // (SortOrder 0) and put every session in it. Drop it so existing users see
        // an empty group strip until they create real categories themselves.
        if (_groups.Count == 1 && _groups[0].Name == "Default" && _groups[0].SortOrder == 0)
        {
            string legacyId = _groups[0].Id;
            foreach (var s in _sessions)
            {
                if (s.GroupId == legacyId) s.GroupId = "";
            }
            _groups.Clear();
        }
    }

    public void PopulateState(AppState state)
    {
        state.Sessions = [.. _sessions];
        state.Groups = [.. _groups];
    }
}
