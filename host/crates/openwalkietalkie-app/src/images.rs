//! Small window-local image cache. The daemon supplies bounded PNG previews;
//! source paths and remote URLs never become GPUI image sources.
use crate::{
    shell::Shell,
    style::palette,
    ui::{self, Btn, Type as _},
};
use base64::{engine::general_purpose::STANDARD, Engine};
use deck_gui_core::UiAction;
use deck_model::{Agent, AgentId, ChatRole};
use gpui::{
    div, img, prelude::*, px, AnyElement, Context, Div, Image, ImageFormat, ImageSource, ObjectFit,
    SharedString, Window,
};
use gpui_component::v_flex;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

#[derive(Default)]
pub struct View {
    owner: Option<AgentId>,
    cache: BTreeMap<String, Option<Arc<Image>>>,
    failed: BTreeSet<String>,
    pub expanded: Option<String>,
}
impl View {
    pub(crate) fn clear(&mut self, cx: &mut gpui::App) {
        for data in self.cache.values().flatten() {
            ImageSource::Image(data.clone()).remove_asset(cx);
        }
        self.cache.clear();
        self.failed.clear();
        self.expanded = None;
    }
    pub(crate) fn preview(owner: AgentId, expanded: bool) -> Self {
        let id = "preview-image".to_string();
        let bytes = include_bytes!("../../../../site/public/assets/screens/channel.png").to_vec();
        Self {
            owner: Some(owner),
            cache: BTreeMap::from([(
                id.clone(),
                Some(Arc::new(Image::from_bytes(ImageFormat::Png, bytes))),
            )]),
            failed: BTreeSet::new(),
            expanded: expanded.then_some(id),
        }
    }
}

impl Shell {
    pub(crate) fn refresh_images(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.deck.images.owner != self.deck.selected {
            self.deck.images.clear(cx);
            self.deck.images = View {
                owner: self.deck.selected.clone(),
                ..Default::default()
            };
        }
        let Some(agent) = self
            .deck
            .selected
            .as_ref()
            .and_then(|id| self.state.mirror().agents.get(id))
        else {
            return;
        };
        let valid: BTreeSet<_> = agent
            .messages
            .iter()
            .flat_map(|m| &m.message.images)
            .map(|i| i.id.clone())
            .collect();
        self.deck.images.cache.retain(|id, data| {
            if valid.contains(id) {
                true
            } else {
                if let Some(data) = data {
                    ImageSource::Image(data.clone()).remove_asset(cx);
                }
                false
            }
        });
        self.deck.images.failed.retain(|id| valid.contains(id));
        if self
            .deck
            .images
            .expanded
            .as_ref()
            .is_some_and(|id| !valid.contains(id))
        {
            self.deck.images.expanded = None;
        }
        let id = agent.id.clone();
        let images: Vec<_> = agent
            .messages
            .iter()
            .rev()
            .flat_map(|m| m.message.images.iter().rev())
            .take(4)
            .filter(|i| !self.deck.images.cache.contains_key(&i.id))
            .map(|i| i.id.clone())
            .collect();
        for image in images {
            self.load_image(id.clone(), image, window, cx);
        }
    }
    fn load_image(
        &mut self,
        agent: AgentId,
        image: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // At most eight 1024px decoded images in the window, independent of history.
        if self.deck.images.cache.len() >= 8 {
            self.deck.images.clear(cx);
        }
        self.deck.images.cache.insert(image.clone(), None);
        self.deck.images.failed.remove(&image);
        self.image_chunk(agent, image, Vec::new(), None, window, cx);
    }
    fn image_chunk(
        &mut self,
        agent: AgentId,
        image: String,
        mut bytes: Vec<u8>,
        expected: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let offset = bytes.len();
        self.ask(
            UiAction::ImageGet {
                agent: agent.clone(),
                image: image.clone(),
                offset,
            },
            window,
            cx,
            move |this, result, window, cx| {
                if this.deck.selected.as_ref() != Some(&agent)
                    || !this.deck.images.cache.contains_key(&image)
                {
                    return;
                }
                let part = result.ok().and_then(|v| {
                    let total = v["total"].as_u64()? as usize;
                    if total > 4 * 1024 * 1024
                        || total == 0
                        || expected.is_some_and(|e| e != total)
                        || v["offset"].as_u64()? as usize != offset
                    {
                        return None;
                    }
                    let data = v["data"].as_str().filter(|s| s.len() <= 64 * 1024)?;
                    let data = STANDARD.decode(data).ok()?;
                    if data.is_empty()
                        || offset + data.len() > total
                        || (offset + data.len() < total && data.len() != 48 * 1024)
                    {
                        return None;
                    }
                    Some((total, data))
                });
                if let Some((total, part)) = part {
                    bytes.extend(part);
                    if bytes.len() < total {
                        this.image_chunk(agent, image, bytes, Some(total), window, cx);
                    } else {
                        this.deck.images.cache.insert(
                            image,
                            Some(Arc::new(Image::from_bytes(ImageFormat::Png, bytes))),
                        );
                    }
                } else {
                    this.deck.images.failed.insert(image);
                }
            },
        );
    }
    pub(crate) fn render_images(&self, agent: &Agent, cx: &Context<Self>) -> Div {
        let p = *palette(cx);
        let mut body = v_flex().gap(px(16.));
        let mut count = 0;
        for message in &agent.messages {
            for image in &message.message.images {
                count += 1;
                let id = image.id.clone();
                let aid = agent.id.clone();
                let who = self.tr(match message.message.role {
                    ChatRole::User => "You",
                    ChatRole::Assistant => "Assistant",
                    ChatRole::Tool => "Tool",
                });
                let label = format!(
                    "{who} · {} · {} × {}",
                    image.label, image.width, image.height
                );
                let mut row = ui::panel(&p)
                    .gap(px(8.))
                    .child(div().ts(12., 18.).text_color(p.muted).child(label));
                if let Some(Some(data)) = self.deck.images.cache.get(&id) {
                    let data = data.clone();
                    row = row.child(
                        div()
                            .id(SharedString::from(format!("image-{id}")))
                            .cursor_pointer()
                            .w_full()
                            .h(px(240.))
                            .relative()
                            .overflow_hidden()
                            .child(
                                img(data)
                                    .absolute()
                                    .size_full()
                                    .object_fit(ObjectFit::Contain),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.deck.images.expanded = Some(id.clone());
                                cx.notify();
                            })),
                    );
                } else {
                    let failed = self.deck.images.failed.contains(&id);
                    let loading = self.deck.images.cache.contains_key(&id) && !failed;
                    row = row.child(
                        Btn::new(
                            SharedString::from(format!("load-{id}")),
                            self.tr(if failed {
                                "Preview unavailable · retry"
                            } else if loading {
                                "Loading image…"
                            } else {
                                "Load image"
                            }),
                        )
                        .disabled(loading)
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.load_image(aid.clone(), id.clone(), window, cx);
                            },
                        )),
                    );
                }
                body = body.child(row);
            }
        }
        if count == 0 {
            div()
        } else {
            ui::section(self.tr("Images"), body, &p)
        }
    }
    pub(crate) fn expanded_image(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let id = self.deck.images.expanded.as_ref()?;
        let Some(Some(data)) = self.deck.images.cache.get(id) else {
            return None;
        };
        Some(
            v_flex()
                .size_full()
                .gap(px(12.))
                .p(px(24.))
                .child(
                    Btn::new("close-image", self.tr("Back to chat")).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.deck.images.expanded = None;
                            cx.notify();
                        },
                    )),
                )
                .child(
                    img(data.clone())
                        .w_full()
                        .flex_1()
                        .min_h(px(0.))
                        .object_fit(ObjectFit::Contain),
                )
                .into_any_element(),
        )
    }
}
