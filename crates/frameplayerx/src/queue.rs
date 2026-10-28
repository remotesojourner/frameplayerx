//! Up next: the videos playing in turn. Opening a video from a list (a
//! folder, a grid, a season) makes that list the queue for X/Y; Play all, a
//! playlist and what you queue yourself also play through, the next video
//! starting when one ends.

use crate::playback::OpenRequest;

#[derive(Default)]
pub struct Queue {
    pub items: Vec<OpenRequest>,
    /// The one playing (or last played).
    pub pos: usize,
    /// The next one starts when one ends.
    pub plays_through: bool,
    /// Counts changes.
    pub version: u64,
}

impl Queue {
    /// The list a video was opened from, at `pos`.
    pub fn set(&mut self, items: Vec<OpenRequest>, pos: usize, plays_through: bool) {
        self.pos = pos.min(items.len().saturating_sub(1));
        self.items = items;
        self.plays_through = plays_through;
        self.version += 1;
    }

    pub fn clear(&mut self) {
        self.set(Vec::new(), 0, false);
    }

    /// Plays the one at `i` next: it becomes the current.
    pub fn jump(&mut self, i: usize) -> Option<&OpenRequest> {
        let req = self.items.get(i)?;
        self.pos = i;
        self.version += 1;
        Some(req)
    }

    /// The one after the current, when it plays through to it.
    pub fn up_next(&self) -> Option<&OpenRequest> {
        self.plays_through
            .then(|| self.items.get(self.pos + 1))
            .flatten()
    }

    /// Queues `req` right after the one playing (`next`) or at the end; a
    /// video opened alone (`playing`) joins the empty queue first. What is
    /// queued by hand plays through.
    pub fn add(&mut self, req: OpenRequest, playing: &OpenRequest, next: bool) {
        if self.items.is_empty() {
            self.items.push(playing.clone());
            self.pos = 0;
        }
        let at = if next { self.pos + 1 } else { self.items.len() };
        self.items.insert(at, req);
        self.plays_through = true;
        self.version += 1;
    }

    /// Takes out the one at `i`, unless it is the one playing.
    pub fn remove(&mut self, i: usize) {
        if i >= self.items.len() || i == self.pos {
            return;
        }
        self.items.remove(i);
        if i < self.pos {
            self.pos -= 1;
        }
        self.version += 1;
    }

    /// Moves the one at `from` to `to`; the one playing stays the current.
    pub fn move_item(&mut self, from: usize, to: usize) {
        if from >= self.items.len() || to >= self.items.len() || from == to {
            return;
        }
        let item = self.items.remove(from);
        self.items.insert(to, item);
        self.pos = if from == self.pos {
            to
        } else if from < self.pos && to >= self.pos {
            self.pos - 1
        } else if from > self.pos && to <= self.pos {
            self.pos + 1
        } else {
            self.pos
        };
        self.version += 1;
    }

    /// Keeps only the one playing.
    pub fn clear_rest(&mut self) {
        if let Some(current) = self.items.get(self.pos).cloned() {
            self.set(vec![current], 0, self.plays_through);
        } else {
            self.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(name: &str) -> OpenRequest {
        OpenRequest {
            location: format!("/v/{name}.mp4"),
            source_id: None,
            entry: None,
            title: None,
            start_at: None,
            ..Default::default()
        }
    }

    fn names(q: &Queue) -> Vec<String> {
        q.items
            .iter()
            .map(|r| {
                r.location
                    .trim_start_matches("/v/")
                    .trim_end_matches(".mp4")
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn a_list_plays_through_only_when_asked() {
        let mut q = Queue::default();
        q.set(vec![req("a"), req("b")], 0, false);
        assert!(
            q.up_next().is_none(),
            "opened from a folder: stops at the end"
        );
        q.set(vec![req("a"), req("b")], 0, true);
        assert_eq!(q.up_next().map(|r| r.location.as_str()), Some("/v/b.mp4"));
        q.pos = 1;
        assert!(q.up_next().is_none(), "the last one");
    }

    #[test]
    fn queuing_by_hand() {
        let mut q = Queue::default();
        // The video open now joins first.
        q.add(req("b"), &req("a"), false);
        assert_eq!(
            (names(&q), q.pos, q.plays_through),
            (vec!["a".into(), "b".into()], 0, true)
        );
        q.add(req("c"), &req("a"), true);
        assert_eq!(names(&q), ["a", "c", "b"]);
    }

    #[test]
    fn editing_keeps_the_one_playing() {
        let mut q = Queue::default();
        q.set(["a", "b", "c", "d"].map(req).to_vec(), 1, true);
        q.remove(1);
        assert_eq!(names(&q), ["a", "b", "c", "d"], "not the one playing");
        q.remove(0);
        assert_eq!(
            (names(&q), q.pos),
            (vec!["b".into(), "c".into(), "d".into()], 0)
        );
        q.move_item(2, 0);
        assert_eq!(
            (names(&q), q.pos),
            (vec!["d".into(), "b".into(), "c".into()], 1)
        );
        q.move_item(1, 2);
        assert_eq!(
            (names(&q), q.pos),
            (vec!["d".into(), "c".into(), "b".into()], 2)
        );
        q.move_item(0, 2);
        assert_eq!(
            (names(&q), q.pos),
            (vec!["c".into(), "b".into(), "d".into()], 1)
        );
        q.clear_rest();
        assert_eq!((names(&q), q.pos), (vec!["b".into()], 0));
    }
}
