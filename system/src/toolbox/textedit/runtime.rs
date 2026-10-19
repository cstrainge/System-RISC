//! Resident TextEdit drawing. Applications supply a window, never a renderer.
use alloc::{string::String, vec::Vec};

use mac_abi::{quickdraw::*, textedit::*, *};

use crate::toolbox::quickdraw::{region::{BooleanOp, Region},
                                retained::RenderCommand,
                                service};

impl super::Desktop
{
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
                self.redraw |= self.cursors.obscure(self.events.mouse);
            }
            return status;
        }
        if call.operation != DRAW
        {
            let status = self.textedit.request_with_fonts(call, input, output, fonts);
            if status == OK && matches!(call.operation, SET_CURSOR_ENABLED | DISPOSE)
            {
                self.redraw = true;
            }
            if status == OK
               && matches!(call.operation, KEY | NAVIGATE)
               && self.owner == self.foreground
            {
                self.redraw |= self.cursors.obscure(self.events.mouse);
            }
            return status;
        }
        self.draw_textedit(call).map_or_else(|status| status, |()| OK)
    }

    fn draw_textedit(&mut self, call: &mut TextEditCall) -> Result<(), Status>
    {
        // Stage a complete retained update before changing any visible drawing.
        // The original port, including its nonrectangular clip, stays untouched.
        let mut port = self.content_mut(call.window.0)?.port.clone();
        let editor = self.textedit.inspect(call.id).ok_or(NOT_FOUND)?;
        let info = editor.info();
        let view = Region::from_rect(info.view);
        port.state.clip = Some(port.state.clip.as_ref().map_or_else(|| view.clone(),
                                                                    |clip| {
                                                                        clip.combine(&view,
                                                                       BooleanOp::Intersection)
                                                                    }));
        let mut commands = Vec::new();
        commands.try_reserve(editor.lines().len().min(512)).map_err(|_| NO_MEMORY)?;
        for (index, line) in editor.lines().iter().enumerate()
        {
            let top = i32::from(info.dest.top) + index as i32 * i32::from(info.line_height);
            if top >= i32::from(info.view.bottom)
               || top + i32::from(info.line_height) <= i32::from(info.view.top)
            {
                continue;
            }
            if commands.len() >= 512
            {
                return Err(NO_MEMORY);
            }
            let y = i32::from(info.dest.top) + index as i32 * i32::from(info.line_height);
            let source = &editor.text()[line.start as usize..line.end as usize];
            let source = source.strip_suffix('\r').unwrap_or(source);
            let mut text = String::new();
            text.try_reserve_exact(source.len()).map_err(|_| NO_MEMORY)?;
            text.push_str(source);
            commands.push(RenderCommand::Text { font: editor.font(),
                                                baseline: Point { h: info.dest
                                                                         .left
                                                                         .saturating_add(1),
                                                                  v: (y + i32::from(info.ascent))
                                                                     as i16 },
                                                text,
                                                color: 0,
                                                port: port.clone() });
        }
        let mut failure = None;
        let mut invert = |bounds: Rect| {
            if failure.is_some()
            {
                return;
            }
            let mut visible = Rect::ZERO;
            crate::toolbox::quickdraw::geometry::sect_rect(bounds, info.view, &mut visible);
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
        if info.active != 0 && info.caret_visible != 0 && info.selection_start == info.selection_end
        {
            invert(editor.caret_rect());
        }
        if let Some(status) = failure
        {
            return Err(status);
        }
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
        self.content_changed(call.window.0);
        let staged = self.pending_content.iter().any(|content| content.id == call.window);
        self.textedit.bind_cursor(call.id, call.window, port, staged)?;
        call.info = info;
        Ok(())
    }
}
