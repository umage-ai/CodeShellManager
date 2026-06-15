using System;

namespace CodeShellManager.Models;

public class SessionGroup
{
    public string Id { get; set; } = Guid.NewGuid().ToString();
    public string Name { get; set; } = "Default";
    public int SortOrder { get; set; } = 0;
    public bool IsExpanded { get; set; } = true;
}
