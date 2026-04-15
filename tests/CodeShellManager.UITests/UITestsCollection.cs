using Xunit;

namespace CodeShellManager.UITests;

/// <summary>
/// Declares the "UITests" xUnit collection with parallelization disabled.
/// All test classes decorated with [Collection("UITests")] run sequentially,
/// preventing multiple AppFixture instances from driving the WPF app simultaneously.
/// </summary>
[CollectionDefinition("UITests", DisableParallelization = true)]
public sealed class UITestsCollection { }
