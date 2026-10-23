//! Resident TextEdit drawing. Applications supply a window, never a renderer.
use alloc::{string::String, sync::Arc, vec::Vec};
use core::fmt::Write;

use mac_abi::{quickdraw::*, textedit::*, *};

use crate::toolbox::quickdraw::{region::{BooleanOp, Region},
                                retained::RenderCommand,
                                service};

impl super::Desktop
{
    pub fn textedit_display_request(&mut self, call: &mut TextEditDisplayCall) -> Status
    {
        self.textedit.display_request(call)
    }

    pub fn textedit_font_request(&mut self, call: &mut TextEditFontCall) -> Status
    {
        crate::kernel::fonts().map_or(NOT_FOUND, |fonts| self.textedit.font_request(call, fonts))
    }

    pub fn textedit_request(&mut self, call: &mut TextEditCall, input: &str, output: &mut [u8])
                            -> Status
    {
        let fallback = crate::toolbox::quickdraw::fonts::FontManager::default();
        let fonts = crate::kernel::fonts().unwrap_or(&fallback);
        if matches!(call.operation, COPY | CUT | PASTE)
        {
            let status = crate::kernel::with_scrap(|scrap| {
                             crate::toolbox::textedit::clipboard::request(&mut self.textedit,
                                                                          scrap, call, input,
                                                                          output, fonts)
                         }).unwrap_or_else(|error| error.status());
            if status == OK
               && matches!(call.operation, CUT | PASTE)
               && self.owner == self.foreground
            {
                self.cursor_dirty |= self.cursors.obscure(self.events.mouse);
            }
            return status;
        }
        if call.operation == BLINK
        {
            return self.blink_textedit(call).map_or_else(|status| status, |()| OK);
        }
        if call.operation != DRAW
        {
            let status = self.textedit.request_with_fonts(call, input, output, fonts);
            if status == OK && matches!(call.operation, SET_CURSOR_ENABLED | DISPOSE)
            {
                self.cursor_dirty = true;
            }
            if status == OK
               && matches!(call.operation, KEY | NAVIGATE)
               && self.owner == self.foreground
            {
                self.cursor_dirty |= self.cursors.obscure(self.events.mouse);
            }
            return status;
        }
        self.draw_textedit(call, fonts).map_or_else(|status| status, |()| OK)
    }

    fn blink_textedit(&mut self, call: &mut TextEditCall) -> Result<(), Status>
    {
        if !self.owns(call.window)
        {
            return Err(ACCESS_DENIED);
        }
        let editor = self.textedit.inspect_mut(call.id).ok_or(NOT_FOUND)?;
        let before = editor.info();
        editor.idle(call.ticks);
        call.info = editor.info();
        call.index = 0;
        if call.info == before
        {
            return Ok(());
        }
        let Some(content) = self.content.iter_mut().find(|c| c.id == call.window)
        else
        {
            call.index = 1;
            return Ok(());
        };
        let Some(region) = content.blink_caret(call.id, before, call.info)
        else
        {
            call.index = 1;
            return Ok(());
        };
        if self.redraw
           || !self.content_damage.is_empty()
           || self.menus.is_tracking()
           || self.app_menu.is_tracking()
           || self.windows.is_tracking()
        {
            self.content_changed_region(call.window.0, region);
            return Ok(());
        }
        let window = self.windows.window(call.window).ok_or(NOT_FOUND)?;
        let mut canvas =
            crate::toolbox::quickdraw::Canvas::new(crate::platform::back_buffer(), 640, 480);
        for band in region.bands()
        {
            for span in &band.spans
            {
                let local = Rect::new(span.left, band.top, span.right, band.bottom);
                crate::trace::record(crate::trace::TEXTEDIT,
                                     self.owner,
                                     3,
                                     (i32::from(local.right) - i32::from(local.left)) as u64,
                                     (i32::from(local.bottom) - i32::from(local.top)) as u64,
                                     0);
                self.content_cache.invert(call.window.0, local);
                let mut screen = local;
                screen.offset(window.bounds.left, window.bounds.top);
                self.windows.for_each_visible_rect(call.window, |visible| {
                                let mut rect = Rect::ZERO;
                                if crate::toolbox::quickdraw::geometry::sect_rect(screen, visible,
                                                                                  &mut rect)
                                {
                                    canvas.set_clip(rect);
                                    canvas.invert_rect(rect);
                                    self.outline_damage.push(rect);
                                }
                            });
            }
        }
        Ok(())
    }

    fn draw_textedit(&mut self, call: &mut TextEditCall,
                     fonts: &crate::toolbox::quickdraw::fonts::FontManager)
                     -> Result<(), Status>
    {
        // Stage a complete retained update before changing any visible drawing.
        // The original port, including its nonrectangular clip, stays untouched.
        let mut port = self.content_mut(call.window.0)?.port.clone();
        let (snapshot_start, snapshot) =
            self.textedit.inspect_mut(call.id).ok_or(NOT_FOUND)?.snapshot()?;
        let editor = self.textedit.inspect(call.id).ok_or(NOT_FOUND)?;
        let info = editor.info();
        let vertical_ink = fonts.vertical_ink_bounds(crate::toolbox::quickdraw::fonts_service::selection(editor.font())).ok().flatten();
        let outer_port = port.clone();
        let view = Region::from_rect(editor.text_view());
        port.state.clip = Some(port.state.clip.as_ref().map_or_else(|| view.clone(),
                                                                    |clip| {
                                                                        clip.combine(&view,
                                                                       BooleanOp::Intersection)
                                                                    }));
        let mut commands = Vec::new();
        let visible = editor.visible_lines();
        commands.try_reserve(visible.len().saturating_mul(2).saturating_add(6).min(512))
                .map_err(|_| NO_MEMORY)?;
        let geometry = editor.display_geometry();
        let mut outer_port = outer_port;
        let view_clip = Region::from_rect(info.view);
        outer_port.state.clip =
            Some(outer_port.state
                           .clip
                           .as_ref()
                           .map_or_else(|| view_clip.clone(),
                                        |clip| clip.combine(&view_clip, BooleanOp::Intersection)));
        let mut fill = |bounds: Rect, color: u32| -> Result<(), Status> {
            if bounds.is_empty()
            {
                return Ok(());
            }
            let mut paint_port = outer_port.clone();
            paint_port.state.foreground = color;
            if let Some(drawing) = service::graphics(&mut paint_port,
                                                     &mut GraphicsCall { operation: QD_SHAPE,
                                                                         shape: SHAPE_RECT,
                                                                         verb: VERB_PAINT,
                                                                         bounds,
                                                                         ..Default::default() },
                                                     &[],
                                                     &[])?
            {
                commands.push(RenderCommand::Graphics(drawing));
            }
            Ok(())
        };
        const SHADE: u32 = 0xeeeeee;
        const RULE: u32 = 0x999999;
        if geometry.gutter_width != 0
        {
            fill(Rect::new(info.view.left, info.view.top, geometry.text_view.left,
                           info.view.bottom),
                 SHADE)?;
            fill(Rect::new(geometry.text_view.left.saturating_sub(1),
                           info.view.top,
                           geometry.text_view.left,
                           info.view.bottom),
                 RULE)?;
        }
        if editor.display_options().ruler_kind != 0
        {
            let x =
                geometry.ruler_x.clamp(i32::from(geometry.text_view.left),
                                       i32::from(geometry.text_view.right)) as i16;
            if geometry.ruler_x < i32::from(geometry.text_view.right)
            {
                fill(Rect::new(x, info.view.top, info.view.right, info.view.bottom), SHADE)?;
                if geometry.ruler_x >= i32::from(geometry.text_view.left)
                {
                    fill(Rect::new(x, info.view.top, x.saturating_add(1), info.view.bottom), RULE)?;
                }
            }
        }
        drop(fill);
        if geometry.gutter_width != 0
        {
            let mut numbers = String::new();
            numbers.try_reserve(visible.len().saturating_add(1) * 6).map_err(|_| NO_MEMORY)?;
            let mut labels = Vec::new();
            labels.try_reserve(visible.len() + 1).map_err(|_| NO_MEMORY)?;
            for row in visible.clone()
            {
                let line = &editor.lines()[row];
                if line.first
                {
                    let start = numbers.len();
                    write!(&mut numbers, "{}", line.logical).map_err(|_| NO_MEMORY)?;
                    labels.push((row, start..numbers.len()));
                }
            }
            if editor.text().is_empty() || editor.text().ends_with('\r')
            {
                let row = editor.lines().len();
                let top = editor.origin_y() + row as i32 * i32::from(info.line_height);
                if top < i32::from(info.view.bottom)
                   && top + i32::from(info.line_height) > i32::from(info.view.top)
                {
                    let start = numbers.len();
                    write!(&mut numbers, "{}", geometry.logical_lines).map_err(|_| NO_MEMORY)?;
                    labels.push((row, start..numbers.len()));
                }
            }
            let numbers = Arc::new(numbers);
            let number_port = Arc::new(outer_port.clone());
            for (row, range) in labels
            {
                let width = editor.number_width(&numbers[range.clone()]);
                commands.push(RenderCommand::Text { vertical_ink, font: editor.font(),
                    baseline: Point { h: (i32::from(geometry.text_view.left) - 5 - width as i32).clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16,
                        v: (editor.origin_y() + row as i32 * i32::from(info.line_height) + i32::from(info.ascent)) as i16 },
                    text: crate::toolbox::quickdraw::retained::TextData::Shared { source: numbers.clone(), range }, color: 0x666666, port: number_port.clone() });
            }
        }
        let text_port = Arc::new(port.clone());
        for (index, line) in
            editor.lines()[visible.clone()].iter()
                                           .enumerate()
                                           .map(|(index, line)| (index + visible.start, line))
        {
            let top = editor.origin_y() + index as i32 * i32::from(info.line_height);
            if top >= i32::from(info.view.bottom)
               || top + i32::from(info.line_height) <= i32::from(info.view.top)
            {
                continue;
            }
            if commands.len() >= 512
            {
                return Err(NO_MEMORY);
            }
            let y = editor.origin_y() + index as i32 * i32::from(info.line_height);
            let (range, baseline_x) = editor.visible_text(line);
            let text =
                crate::toolbox::quickdraw::retained::TextData::Shared { source: snapshot.clone(),
                                                                        range:  range.start
                                                                                - snapshot_start
                                                                                ..range.end
                                                                                  - snapshot_start };
            commands.push(RenderCommand::Text { vertical_ink,
                                                font: editor.font(),
                                                baseline: Point { h: baseline_x,
                                                                  v: (y + i32::from(info.ascent))
                                                                     as i16 },
                                                text,
                                                color: 0,
                                                port: text_port.clone() });
        }
        let mut failure = None;
        let mut invert = |bounds: Rect| {
            if failure.is_some()
            {
                return;
            }
            let mut visible = Rect::ZERO;
            crate::toolbox::quickdraw::geometry::sect_rect(bounds,
                                                           editor.text_view(),
                                                           &mut visible);
            if visible.is_empty()
            {
                return;
            }
            if commands.len() >= 512 || commands.try_reserve(1).is_err()
            {
                failure = Some(NO_MEMORY);
                return;
            }
            match service::graphics(&mut port,
                                    &mut GraphicsCall { operation: QD_SHAPE,
                                                        shape: SHAPE_RECT,
                                                        verb: VERB_INVERT,
                                                        bounds,
                                                        ..Default::default() },
                                    &[],
                                    &[])
            {
                Ok(Some(drawing)) => commands.push(RenderCommand::Graphics(drawing)),
                Ok(None) =>
                {}
                Err(status) => failure = Some(status)
            }
        };
        if info.active != 0 && info.selection_start != info.selection_end
        {
            editor.selection_rects(&mut invert);
        }
        drop(invert);
        commands.try_reserve(1).map_err(|_| NO_MEMORY)?;
        commands.push(RenderCommand::Caret { editor: call.id,
                                             info,
                                             bounds: editor.caret_rect(),
                                             port: port.clone() });
        if let Some(status) = failure
        {
            return Err(status);
        }
        crate::trace::record(crate::trace::TEXTEDIT, self.owner, 2, commands.len() as u64, 0, 0);
        let bytes = commands.iter().map(RenderCommand::storage).sum::<usize>();
        if self.retained_bytes() + bytes > super::MAX_RETAINED_BYTES
        {
            return Err(NO_MEMORY);
        }
        let content = self.content_mut(call.window.0)?;
        if content.commands.len() + commands.len() > 512
        {
            return Err(NO_MEMORY);
        }
        content.commands.try_reserve(commands.len()).map_err(|_| NO_MEMORY)?;
        content.commands.extend(commands);
        self.content_changed_rect(call.window.0, info.view);
        let staged = self.pending_content.iter().any(|content| content.id == call.window);
        self.textedit.bind_cursor(call.id, call.window, port, staged)?;
        call.info = info;
        Ok(())
    }
}
