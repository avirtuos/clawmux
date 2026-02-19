//! TaskStore: in-memory task cache with file watching.
//!
//! Loads task markdown files from `./tasks/` or `./docs/tasks/`, watches for
//! external modifications via the `notify` crate, and maintains an in-memory
//! cache keyed by `TaskId`.
//! Task 2.3 implements the full TaskStore.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub mod models;
pub mod parser;
pub mod writer;

#[allow(unused_imports)]
pub use models::{Question, Story, Task, TaskId, TaskStatus, WorkLogEntry};
#[allow(unused_imports)]
pub use writer::write_task;

use crate::error::ClawdMuxError;

/// In-memory store for all loaded stories and tasks.
///
/// Discovers task files from `./tasks/` or `./docs/tasks/` on startup.
/// Caches parsed tasks and provides CRUD-style access by [`TaskId`].
#[allow(dead_code)]
pub struct TaskStore {
    tasks: HashMap<TaskId, Task>,
}

#[allow(dead_code)]
impl TaskStore {
    /// Creates an empty task store.
    pub fn new() -> Self {
        TaskStore {
            tasks: HashMap::new(),
        }
    }

    /// Discovers and loads all `*.md` files from the project task directories.
    ///
    /// Scans `{project_root}/tasks/` first; if that directory does not exist,
    /// falls back to `{project_root}/docs/tasks/`. Files that fail to parse are
    /// skipped with a warning rather than aborting the load.
    ///
    /// Returns the number of task files successfully loaded.
    ///
    /// # Errors
    ///
    /// Returns [`ClawdMuxError::Io`] if the chosen task directory cannot be read.
    pub fn load_from_disk(&mut self, project_root: &Path) -> crate::error::Result<usize> {
        let tasks_dir = {
            let primary = project_root.join("tasks");
            if primary.exists() {
                primary
            } else {
                project_root.join("docs").join("tasks")
            }
        };

        let entries = std::fs::read_dir(&tasks_dir)?;

        let mut loaded = 0usize;
        for entry in entries {
            let entry = entry?;
            let path: PathBuf = entry.path();

            // Only process .md files.
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }

            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("Failed to read task file {}: {}", path.display(), e);
                    continue;
                }
            };

            match parser::parse_task(&content, path.clone()) {
                Ok(task) => {
                    self.tasks.insert(task.id.clone(), task);
                    loaded += 1;
                }
                Err(e) => {
                    tracing::warn!("Failed to parse task file {}: {}", path.display(), e);
                }
            }
        }

        Ok(loaded)
    }

    /// Returns all stories, each containing their tasks sorted by task name.
    ///
    /// Stories are sorted by story name. Tasks within each story are sorted using
    /// the numeric `<story>.<task>` ordering defined by [`Story::sorted_tasks`].
    pub fn stories(&self) -> Vec<Story> {
        let mut by_story: HashMap<&str, Vec<&Task>> = HashMap::new();
        for task in self.tasks.values() {
            by_story
                .entry(task.story_name.as_str())
                .or_default()
                .push(task);
        }

        let mut story_names: Vec<&str> = by_story.keys().copied().collect();
        story_names.sort();

        story_names
            .into_iter()
            .map(|name| {
                let tasks: Vec<Task> = by_story[name].iter().map(|t| (*t).clone()).collect();
                let story = Story {
                    name: name.to_string(),
                    tasks,
                };
                // Re-order tasks using the story's own numeric sort.
                let sorted_tasks: Vec<Task> = story.sorted_tasks().into_iter().cloned().collect();
                Story {
                    name: name.to_string(),
                    tasks: sorted_tasks,
                }
            })
            .collect()
    }

    /// Returns a reference to the task with the given ID, or `None` if not present.
    pub fn get(&self, id: &TaskId) -> Option<&Task> {
        self.tasks.get(id)
    }

    /// Returns a mutable reference to the task with the given ID, or `None` if not present.
    pub fn get_mut(&mut self, id: &TaskId) -> Option<&mut Task> {
        self.tasks.get_mut(id)
    }

    /// Inserts or replaces a task in the store, keyed by its `id`.
    pub fn insert(&mut self, task: Task) {
        self.tasks.insert(task.id.clone(), task);
    }

    /// Serializes the task to its markdown file on disk, then updates the store.
    ///
    /// # Errors
    ///
    /// Returns [`ClawdMuxError::Internal`] if the task ID is not in the store.
    /// Returns [`ClawdMuxError::Encode`] or [`ClawdMuxError::Io`] on write failure.
    pub fn persist(&mut self, id: &TaskId) -> crate::error::Result<()> {
        let task = self
            .tasks
            .get(id)
            .ok_or_else(|| ClawdMuxError::Internal(format!("persist: task not found: {id}")))?;
        let content = writer::write_task(task)?;
        let file_path = task.file_path.clone();
        std::fs::write(&file_path, content)?;
        Ok(())
    }

    /// Reloads a single task from disk, replacing the in-memory copy.
    ///
    /// # Errors
    ///
    /// Returns [`ClawdMuxError::Internal`] if the task ID is not in the store.
    /// Returns [`ClawdMuxError::Io`] or [`ClawdMuxError::Parse`] on read/parse failure.
    pub fn reload(&mut self, id: &TaskId) -> crate::error::Result<()> {
        let file_path = self
            .tasks
            .get(id)
            .ok_or_else(|| ClawdMuxError::Internal(format!("reload: task not found: {id}")))?
            .file_path
            .clone();
        let content = std::fs::read_to_string(&file_path)?;
        let task = parser::parse_task(&content, file_path)?;
        self.tasks.insert(task.id.clone(), task);
        Ok(())
    }

    /// Returns the total number of tasks across all stories.
    pub fn task_count(&self) -> usize {
        self.tasks.len()
    }
}

impl Default for TaskStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::*;
    use crate::tasks::models::TaskStatus;

    /// Writes `content` to `dir/filename` and returns the file path.
    fn write_file(dir: &Path, filename: &str, content: &str) -> PathBuf {
        let path = dir.join(filename);
        std::fs::write(&path, content).unwrap();
        path
    }

    /// Returns a minimal valid task markdown string.
    fn minimal_md(story: &str, task: &str) -> String {
        format!("Story: {story}\nTask: {task}\nStatus: OPEN\n\n## Description\n\nA description.\n")
    }

    #[test]
    fn test_load_from_disk_tasks_dir() {
        let tmp = TempDir::new().unwrap();
        let tasks_dir = tmp.path().join("tasks");
        std::fs::create_dir(&tasks_dir).unwrap();

        write_file(&tasks_dir, "1.1.md", &minimal_md("1. Story", "1.1"));
        write_file(&tasks_dir, "1.2.md", &minimal_md("1. Story", "1.2"));

        let mut store = TaskStore::new();
        let count = store.load_from_disk(tmp.path()).unwrap();

        assert_eq!(count, 2);
        assert_eq!(store.task_count(), 2);
    }

    #[test]
    fn test_load_from_disk_docs_tasks_fallback() {
        let tmp = TempDir::new().unwrap();
        let docs_tasks = tmp.path().join("docs").join("tasks");
        std::fs::create_dir_all(&docs_tasks).unwrap();

        write_file(&docs_tasks, "1.1.md", &minimal_md("1. Story", "1.1"));

        let mut store = TaskStore::new();
        let count = store.load_from_disk(tmp.path()).unwrap();

        assert_eq!(count, 1);
        assert_eq!(store.task_count(), 1);
    }

    #[test]
    fn test_stories_grouping() {
        let tmp = TempDir::new().unwrap();
        let tasks_dir = tmp.path().join("tasks");
        std::fs::create_dir(&tasks_dir).unwrap();

        write_file(&tasks_dir, "1.1.md", &minimal_md("1. Foo", "1.1"));
        write_file(&tasks_dir, "1.2.md", &minimal_md("1. Foo", "1.2"));
        write_file(&tasks_dir, "2.1.md", &minimal_md("2. Bar", "2.1"));

        let mut store = TaskStore::new();
        store.load_from_disk(tmp.path()).unwrap();

        let stories = store.stories();
        assert_eq!(stories.len(), 2);
        assert_eq!(stories[0].name, "1. Foo");
        assert_eq!(stories[0].tasks.len(), 2);
        assert_eq!(stories[1].name, "2. Bar");
        assert_eq!(stories[1].tasks.len(), 1);
    }

    #[test]
    fn test_persist_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let tasks_dir = tmp.path().join("tasks");
        std::fs::create_dir(&tasks_dir).unwrap();

        write_file(&tasks_dir, "1.1.md", &minimal_md("1. Story", "1.1"));

        let mut store = TaskStore::new();
        store.load_from_disk(tmp.path()).unwrap();

        // Find the loaded task's id and mutate its status.
        let id = store.tasks.keys().next().unwrap().clone();
        store.get_mut(&id).unwrap().status = TaskStatus::Completed;
        store.persist(&id).unwrap();

        // Read the file from disk and re-parse it independently.
        let file_path = store.get(&id).unwrap().file_path.clone();
        let content = std::fs::read_to_string(&file_path).unwrap();
        let reparsed = parser::parse_task(&content, file_path).unwrap();
        assert_eq!(reparsed.status, TaskStatus::Completed);
    }

    #[test]
    fn test_reload_reflects_disk_change() {
        let tmp = TempDir::new().unwrap();
        let tasks_dir = tmp.path().join("tasks");
        std::fs::create_dir(&tasks_dir).unwrap();

        let file_path = write_file(&tasks_dir, "1.1.md", &minimal_md("1. Story", "1.1"));

        let mut store = TaskStore::new();
        store.load_from_disk(tmp.path()).unwrap();

        let id = store.tasks.keys().next().unwrap().clone();
        assert_eq!(store.get(&id).unwrap().status, TaskStatus::Open);

        // Overwrite the file externally with a different status.
        let updated = minimal_md("1. Story", "1.1").replace("Status: OPEN", "Status: COMPLETED");
        std::fs::write(&file_path, updated).unwrap();

        store.reload(&id).unwrap();
        assert_eq!(store.get(&id).unwrap().status, TaskStatus::Completed);
    }

    #[test]
    fn test_get_missing_returns_none() {
        let store = TaskStore::new();
        let id = TaskId::from_path("tasks/nonexistent.md");
        assert!(store.get(&id).is_none());
    }

    #[test]
    fn test_task_count() {
        let tmp = TempDir::new().unwrap();
        let tasks_dir = tmp.path().join("tasks");
        std::fs::create_dir(&tasks_dir).unwrap();

        write_file(&tasks_dir, "1.1.md", &minimal_md("1. Story", "1.1"));
        write_file(&tasks_dir, "1.2.md", &minimal_md("1. Story", "1.2"));
        write_file(&tasks_dir, "1.3.md", &minimal_md("1. Story", "1.3"));

        let mut store = TaskStore::new();
        store.load_from_disk(tmp.path()).unwrap();

        assert_eq!(store.task_count(), 3);
    }

    #[test]
    fn test_skips_unparseable_files() {
        let tmp = TempDir::new().unwrap();
        let tasks_dir = tmp.path().join("tasks");
        std::fs::create_dir(&tasks_dir).unwrap();

        write_file(&tasks_dir, "valid.md", &minimal_md("1. Story", "1.1"));
        write_file(&tasks_dir, "invalid.md", "this is not a valid task file\n");

        let mut store = TaskStore::new();
        let count = store.load_from_disk(tmp.path()).unwrap();

        assert_eq!(count, 1);
        assert_eq!(store.task_count(), 1);
    }
}
