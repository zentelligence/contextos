//! Named points inside a read at which a unit test can change the file, so races between inspecting a path, opening
//! it, and reading it can be exercised deterministically. Production reads pass a hook that ignores every stage.

/// A point in a read, in the order a read reaches them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReadStage {
    /// The path has been inspected as a regular file and has not been opened yet.
    Inspected,
    /// The file is open and its handle metadata has been taken; no content has been read.
    Opened,
    /// The content has been read and the handle metadata has not been read again.
    ContentRead,
}

/// The hook a read calls at each [`ReadStage`].
pub(crate) type ReadHook<'a> = &'a mut dyn FnMut(ReadStage);
