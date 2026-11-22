//! Hands-free navigation: the head points, and holding the dot on a control
//! for the hold time presses it, as a trigger pulled there would.

use crate::world::{Panel, ray_to_plane};
use egui::{Context, Event, Id, Modifiers, PointerButton, Pos2, Rect};
use fp_xr::Hand;
use glam::{Mat4, Quat, Vec2, Vec3};

/// Seconds both controllers must be left alone, neither pressed nor moved,
/// before the head points again.
const CONTROLLER_SECS: f32 = 3.0;
/// How far a controller's ray turns to count as moved: held in a hand, it
/// soon turns further; lying still, it stays well within this.
const CONTROLLER_MOVE_DEG: f32 = 1.5;
/// How far, in panel points, a control may move and keep its count: any
/// further and a list has scrolled it from under the dot.
const MOVE_TOLERANCE: f32 = 3.0;
/// How far the head may turn during a hold. Further, and the dot is passing
/// over the control rather than resting on it, so the count starts again
/// where it is: sweeping across a wide card or row presses nothing.
const STILL_DEG: f32 = 2.0;
/// The most one frame adds to a count, so a stalled frame can't finish one.
const MAX_STEP_SECS: f32 = 0.05;
/// After a press nothing counts until the head has turned this far and the
/// dot is off what it pressed, so what a press brings up under a still dot
/// (a page, the control bar) isn't pressed in turn.
const SPENT_DEG: f32 = 2.0;

#[derive(Default)]
pub struct HeadPointer {
    controller_secs: f32,
    /// Where each controller's ray pointed when it last counted as moved.
    controller_dirs: [Option<Vec3>; 2],
    target: Option<Target>,
    spent: Option<Spent>,
    click: Option<Click>,
}

/// The control the dot is held on.
#[derive(Clone, Copy)]
struct Target {
    panel: usize,
    id: Id,
    /// Where it was, and where the head pointed, when the count started.
    rect: Rect,
    dir: Vec3,
    secs: f32,
}

/// What was last pressed, where it was in the room, and where the head
/// pointed then.
struct Spent {
    pose: Mat4,
    half: Vec2,
    dir: Vec3,
}

/// A press's two halves go a frame apart, as a trigger's do: a slider moves
/// while the button is down.
#[derive(Clone, Copy)]
enum Click {
    Press(usize, Pos2),
    Release(usize, Pos2),
}

impl HeadPointer {
    /// The head pose to point with: `head`, unless a controller was pressed
    /// or moved a moment ago, or `held` (a press or a panel move is under
    /// way).
    pub fn aim(
        &mut self,
        head: Option<(Vec3, Quat)>,
        hands: &[Hand; 2],
        held: bool,
        dt: f32,
    ) -> Option<(Vec3, Quat)> {
        let mut moved = false;
        for (h, last) in hands.iter().zip(&mut self.controller_dirs) {
            let Some((_, rot)) = h.aim else {
                *last = None;
                continue;
            };
            let dir = rot * Vec3::NEG_Z;
            match *last {
                Some(l) if l.angle_between(dir) < CONTROLLER_MOVE_DEG.to_radians() => {}
                // Coming into view isn't a move; turning once there is.
                l => {
                    moved |= l.is_some();
                    *last = Some(dir);
                }
            }
        }
        if held || moved || hands.iter().any(in_use) {
            self.controller_secs = CONTROLLER_SECS;
        } else {
            self.controller_secs = (self.controller_secs - dt.max(0.0)).max(0.0);
        }
        head.filter(|_| self.controller_secs == 0.0)
    }

    /// Sends the next half of a press to its panel; true on the frame it
    /// goes down, when the dot mustn't move from it. A panel gone meanwhile
    /// keeps its events until it shows again, so it gets no press then, and
    /// a let-go off it, which isn't a click.
    pub fn send_click(&mut self, panels: &mut [&mut Panel]) -> bool {
        match self.click.take() {
            Some(Click::Press(i, pos)) if panels[i].visible => {
                panels[i].push(Event::PointerMoved(pos));
                panels[i].push(button(pos, true));
                self.click = Some(Click::Release(i, pos));
                true
            }
            Some(Click::Release(i, pos)) => {
                let pos = if panels[i].visible {
                    pos
                } else {
                    Pos2::new(-1.0, -1.0)
                };
                panels[i].push(button(pos, false));
                false
            }
            _ => false,
        }
    }

    /// Counts the hold on the control the dot is on (`hit`: its panel and
    /// point), as that panel's last pass found it; true when the hold
    /// presses it.
    pub fn hold(
        &mut self,
        panels: &mut [&mut Panel],
        (origin, rot): (Vec3, Quat),
        hit: Option<(usize, Pos2)>,
        dt: f32,
        hold_secs: f32,
    ) -> bool {
        let dir = rot * Vec3::NEG_Z;
        if self.spent.as_ref().is_some_and(|s| s.holds(origin, dir)) {
            self.target = None;
            show_ring(panels, None);
            return false;
        }
        self.spent = None;
        let under = hit.and_then(|(i, point)| {
            let (widget, seen) = panels[i]
                .ctx
                .viewport(|vp| (vp.hits.click, vp.input.pointer.latest_pos()));
            // Only a pass that saw the dot where it is now says what is
            // under it.
            (seen == Some(point)).then_some(())?;
            widget.map(|w| (i, w.id, w.interact_rect, point))
        });
        let Some((panel, id, rect, point)) = under else {
            self.target = None;
            show_ring(panels, None);
            return false;
        };
        let target = match self.target.filter(|t| {
            t.panel == panel
                && t.id == id
                && t.rect.min.distance(rect.min) <= MOVE_TOLERANCE
                && t.rect.max.distance(rect.max) <= MOVE_TOLERANCE
                && t.dir.angle_between(dir) <= STILL_DEG.to_radians()
        }) {
            Some(t) => Target {
                secs: t.secs + dt.clamp(0.0, MAX_STEP_SECS),
                ..t
            },
            None => Target {
                panel,
                id,
                rect,
                dir,
                secs: 0.0,
            },
        };
        if target.secs >= hold_secs {
            self.target = None;
            self.spent = Some(Spent::of(panels[panel], rect, dir));
            self.click = Some(Click::Press(panel, point));
            show_ring(panels, None);
            return true;
        }
        self.target = Some(target);
        show_ring(panels, Some((panel, target.secs / hold_secs)));
        false
    }

    /// The dot is on a control and counting.
    pub fn holding(&self) -> bool {
        self.target.is_some()
    }

    /// Forgets the count and what was pressed, as when a controller takes
    /// over. A press already sent still gets its let-go.
    pub fn reset(&mut self, panels: &mut [&mut Panel]) {
        self.target = None;
        self.spent = None;
        show_ring(panels, None);
    }
}

impl Spent {
    fn of(panel: &Panel, rect: Rect, dir: Vec3) -> Self {
        let pts = panel.points();
        let c = rect.center();
        let at = Vec3::new(
            (c.x / pts.x - 0.5) * panel.size.x,
            (0.5 - c.y / pts.y) * panel.size.y,
            0.0,
        );
        Spent {
            pose: panel.pose * Mat4::from_translation(at),
            half: Vec2::new(
                rect.width() / pts.x * panel.size.x,
                rect.height() / pts.y * panel.size.y,
            ) / 2.0,
            dir,
        }
    }

    fn holds(&self, origin: Vec3, dir: Vec3) -> bool {
        dir.angle_between(self.dir) < SPENT_DEG.to_radians()
            || ray_to_plane(self.pose, origin, dir)
                .is_some_and(|(_, p)| p.x.abs() <= self.half.x && p.y.abs() <= self.half.y)
    }
}

/// The ring round the dot on the panel `ring` names, filled that far.
fn show_ring(panels: &mut [&mut Panel], ring: Option<(usize, f32)>) {
    for (i, p) in panels.iter_mut().enumerate() {
        let hold = ring.filter(|r| r.0 == i).map(|r| r.1);
        if p.hold != hold {
            p.hold = hold;
            p.request_repaint();
        }
    }
}

fn button(pos: Pos2, pressed: bool) -> Event {
    Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    }
}

/// Anything pressed, pulled or pushed: a controller merely tracked, lying
/// still, doesn't take over.
fn in_use(h: &Hand) -> bool {
    h.trigger > 0.2
        || h.squeeze > 0.2
        || h.stick.length_squared() > 0.2 * 0.2
        || h.stick_click
        || h.menu
        || h.shoulder
        || h.south
        || h.north
        || h.east
        || h.west
}

fn flag() -> Id {
    Id::new("hands-free")
}

/// Hands-free navigation is on, for the widgets that add controls for it.
pub fn enabled(ctx: &Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(flag())).unwrap_or(false)
}

pub fn set_enabled(ctx: &Context, on: bool) {
    ctx.data_mut(|d| d.insert_temp(flag(), on));
}

/// A list on a panel that scrolls, as the panel's last pass showed it: what
/// the keys beside the panel scroll (`ui::widgets::scroll_strip`).
#[derive(Clone, Copy, Debug)]
pub struct ScrollList {
    pub id: Id,
    /// 0 sideways, 1 up and down.
    pub axis: usize,
    /// Where it shows, in the panel's points.
    pub rect: Rect,
    /// Its layer: a drawer's list is over the page's.
    pub order: egui::Order,
    pub can_back: bool,
    pub can_on: bool,
}

fn lists_id() -> Id {
    Id::new("hands-free-lists")
}

fn scroll_id() -> Id {
    Id::new("hands-free-scroll")
}

/// Forgets the lists noted; each pass notes those it shows.
pub fn clear_lists(ctx: &Context) {
    ctx.data_mut(|d| d.remove::<Vec<ScrollList>>(lists_id()));
}

pub fn note_list(ctx: &Context, list: ScrollList) {
    ctx.data_mut(|d| {
        d.get_temp_mut_or_default::<Vec<ScrollList>>(lists_id())
            .push(list);
    });
}

/// The lists the panel's last pass showed with more than fits.
pub fn lists(ctx: &Context) -> Vec<ScrollList> {
    ctx.data(|d| d.get_temp::<Vec<ScrollList>>(lists_id()))
        .unwrap_or_default()
}

/// Asks list `id` to scroll a page back (`dir` -1) or on (1) on its next
/// pass.
pub fn scroll(ctx: &Context, id: Id, dir: f32) {
    ctx.data_mut(|d| d.insert_temp(scroll_id(), (id, dir)));
}

/// The page list `id` was asked to scroll, if any; asked once.
pub fn take_scroll(ctx: &Context, id: Id) -> Option<f32> {
    ctx.data_mut(|d| {
        let (asked, dir) = d.get_temp::<(Id, f32)>(scroll_id())?;
        (asked == id).then(|| {
            d.remove::<(Id, f32)>(scroll_id());
            dir
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::Pointer;

    fn panel() -> Panel {
        let mut p = Panel::new([800, 400], Vec2::new(1.6, 0.8), 1.0);
        p.pose = Mat4::from_translation(Vec3::new(0.0, 0.0, -2.0));
        p.visible = true;
        p
    }

    /// The head looking at (x, y) in the points of [`panel`].
    fn look(x: f32, y: f32) -> (Vec3, Quat) {
        let dir = Vec3::new((x - 400.0) / 500.0, (200.0 - y) / 500.0, -2.0).normalize();
        (Vec3::ZERO, Quat::from_rotation_arc(Vec3::NEG_Z, dir))
    }

    /// One frame as the app runs it: the dot routed, the panel's pass, then
    /// the hold counted. Returns whether the hold pressed.
    struct Frame {
        p: Panel,
        ptr: Pointer,
        head: HeadPointer,
        time: f64,
    }

    impl Frame {
        fn new() -> Self {
            Frame {
                p: panel(),
                ptr: Pointer::default(),
                head: HeadPointer::default(),
                time: 0.0,
            }
        }

        fn run(&mut self, at: (f32, f32), mut ui: impl FnMut(&mut egui::Ui)) -> bool {
            let aim = look(at.0, at.1);
            let pressing = self.head.send_click(&mut [&mut self.p]);
            let hit = self.ptr.route_head(&mut [&mut self.p], aim, !pressing);
            self.time += 0.011;
            let input = self.p.input(self.time);
            let _ = self.p.ctx.run(input, |ctx| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ctx, &mut ui);
            });
            self.head.hold(&mut [&mut self.p], aim, hit, 0.05, 0.5)
        }
    }

    fn button_at(ui: &mut egui::Ui, x: f32, clicks: &mut u32) {
        let rect = Rect::from_min_size(egui::pos2(x, 150.0), egui::vec2(100.0, 100.0));
        if ui
            .interact(rect, Id::new(("button", x as i32)), egui::Sense::click())
            .clicked()
        {
            *clicks += 1;
        }
    }

    #[test]
    fn a_hold_clicks_once_and_not_again_until_the_dot_leaves() {
        let mut f = Frame::new();
        let mut clicks = 0;
        let mut pressed = 0;
        for _ in 0..40 {
            pressed += f.run((150.0, 200.0), |ui| button_at(ui, 100.0, &mut clicks)) as u32;
        }
        assert_eq!((pressed, clicks), (1, 1), "a held dot presses once");
        // Off it and back: another press.
        for _ in 0..3 {
            f.run((400.0, 200.0), |ui| button_at(ui, 100.0, &mut clicks));
        }
        for _ in 0..20 {
            f.run((150.0, 200.0), |ui| button_at(ui, 100.0, &mut clicks));
        }
        assert_eq!(clicks, 2);
    }

    #[test]
    fn what_a_press_brings_up_under_a_still_dot_isnt_pressed() {
        let mut f = Frame::new();
        let (mut first, mut second) = (0, 0);
        let mut opened = false;
        for _ in 0..60 {
            f.run((150.0, 200.0), |ui| {
                if opened {
                    // A new page: another control where the first was.
                    let rect =
                        Rect::from_min_size(egui::pos2(110.0, 160.0), egui::vec2(80.0, 80.0));
                    if ui
                        .interact(rect, Id::new("other"), egui::Sense::click())
                        .clicked()
                    {
                        second += 1;
                    }
                } else {
                    button_at(ui, 100.0, &mut first);
                    opened = first > 0;
                }
            });
        }
        assert_eq!((first, second), (1, 0));
    }

    #[test]
    fn a_control_that_slides_from_under_the_dot_starts_its_count_again() {
        let mut f = Frame::new();
        let mut clicks = 0;
        // 0.5 s needs ten frames; it moves every eighth.
        for i in 0..40 {
            let x = 100.0 + (i / 8) as f32 * 8.0;
            f.run((150.0, 200.0), |ui| button_at(ui, x, &mut clicks));
        }
        assert_eq!(clicks, 0);
    }

    #[test]
    fn a_dot_sweeping_across_a_wide_control_presses_nothing() {
        let mut f = Frame::new();
        let mut clicks = 0;
        let wide = |ui: &mut egui::Ui, clicks: &mut u32| {
            let rect = Rect::from_min_max(egui::pos2(50.0, 150.0), egui::pos2(750.0, 250.0));
            if ui
                .interact(rect, Id::new("row"), egui::Sense::click())
                .clicked()
            {
                *clicks += 1;
            }
        };
        // 20 points a frame, about a degree: the half second it needs to
        // rest goes by on the row.
        for i in 0..30 {
            let x = 100.0 + i as f32 * 20.0;
            f.run((x, 200.0), |ui| wide(ui, &mut clicks));
        }
        assert_eq!(clicks, 0, "passing over it");
        for _ in 0..20 {
            f.run((400.0, 200.0), |ui| wide(ui, &mut clicks));
        }
        assert_eq!(clicks, 1, "resting on it");
    }

    #[test]
    fn using_a_controller_takes_over_but_one_lying_tracked_doesnt() {
        let head = Some((Vec3::ZERO, Quat::IDENTITY));
        let mut nav = HeadPointer::default();
        let mut hands = [Hand::default(); 2];
        hands[0].active = true;
        hands[0].aim = head;
        assert_eq!(nav.aim(head, &hands, false, 0.01), head);
        hands[0].trigger = 0.8;
        assert_eq!(nav.aim(head, &hands, false, 0.01), None);
        hands[0].trigger = 0.0;
        assert_eq!(nav.aim(head, &hands, false, 2.0), None);
        assert_eq!(nav.aim(head, &hands, false, 1.1), head);
    }

    #[test]
    fn a_controller_held_and_moving_keeps_the_head_from_pointing() {
        let head = Some((Vec3::ZERO, Quat::IDENTITY));
        let mut nav = HeadPointer::default();
        let mut hands = [Hand::default(); 2];
        // Pointing about without pressing anything, a degree a step.
        for i in 0..10 {
            let turn = Quat::from_rotation_y((i as f32).to_radians());
            hands[1].aim = Some((Vec3::ZERO, turn));
            let aim = nav.aim(head, &hands, false, 0.5);
            assert!(i < 2 || aim.is_none(), "step {i}");
        }
        // Put down: still for long enough, and the head points again.
        assert_eq!(nav.aim(head, &hands, false, 2.0), None);
        assert_eq!(nav.aim(head, &hands, false, 1.1), head);
    }
}
