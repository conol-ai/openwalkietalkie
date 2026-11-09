//! The same account-bound meters that feed the physical Usage face.
use crate::{
    present,
    shell::Shell,
    style::palette,
    ui::{self, Type as _},
};
use gpui::{div, prelude::*, px, relative, Context, Div, Stateful};
use gpui_component::{h_flex, v_flex};

impl Shell {
    pub(crate) fn render_usage(&self, dim: bool, cx: &Context<Self>) -> Stateful<Div> {
        let p = *palette(cx);
        let usage = self.state.mirror().usage.as_ref();
        let subtitle = if dim {
            self.tr("Disconnected · showing the last readings").to_string()
        } else if let Some(at) = usage.and_then(|u| u.at) {
            let age = present::age(present::now(), at);
            if age == "now" { self.tr("Updates automatically · received just now").into() }
            else { self.fmt("Updates automatically · received {0} ago", &[&(age).to_string()]) }
        } else {
            self.tr("Limits for the active Claude and Codex accounts").into()
        };
        let mut body = v_flex().gap(px(16.)).child(ui::page_header(
            self.tr("Usage"),
            Some(div().child(subtitle).into_any_element()),
            Vec::new(),
            &p,
        ));
        if let Some(usage) = usage.filter(|u| !u.items.is_empty()) {
            for item in &usage.items {
                let percent = item.percent.min(100);
                let color = if dim {
                    p.muted
                } else if percent >= 95 {
                    p.danger
                } else if percent >= 80 {
                    p.warning_muted
                } else {
                    p.accent
                };
                body = body.child(
                    ui::panel(&p)
                        .gap(px(8.))
                        .child(
                            h_flex()
                                .justify_between()
                                .gap(px(12.))
                                .child(div().ts(14., 20.).medium().child(item.name.clone()))
                                .child(
                                    div()
                                        .ts(14., 20.)
                                        .text_color(color)
                                        .child(self.fmt("{0}% used", &[&(percent).to_string()])),
                                ),
                        )
                        .child(
                            div().ts(12., 18.).text_color(p.muted).child(
                                item.account
                                    .clone()
                                    .unwrap_or_else(|| self.tr("Account unavailable").into()),
                            ),
                        )
                        .child(
                            div().w_full().h(px(5.)).rounded_full().bg(p.border).child(
                                div()
                                    .h_full()
                                    .w(relative(percent as f32 / 100.))
                                    .rounded_full()
                                    .bg(color),
                            ),
                        )
                        .child(div().ts(12., 18.).text_color(p.muted).child(
                            if item.reset.is_empty() {
                                self.tr("Reset time unavailable").into()
                            } else {
                                item.reset.clone()
                            },
                        ))
                        .when_some(item.reset_tip.as_ref().filter(|tip| !dim && !tip.is_empty()), |panel, tip| {
                            panel.child(div().ts(12., 18.).text_color(p.warning_muted).child(tip.clone()))
                        }),
                );
            }
        } else {
            body = body.child(
                ui::panel(&p).child(
                    div()
                        .text_color(p.muted)
                        .child(self.tr("Waiting for usage data from your signed-in accounts.")),
                ),
            );
        }
        ui::page(
            "usage",
            &self.page_scroll,
            self.content_width,
            ui::PAGE_MAX,
            body,
        )
    }
}
