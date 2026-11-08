
use std::{ env::current_dir, rc::Rc };

use reedline::Color;

use crate::{ runtime::color::TtyColorMode,
             language::{ native::{ NativeFunction, NativeVisibility },
                         data::{ scoped_variables::{ ScopedValue, ValueVisibility },
                                 methods::MethodDefinition, value::{ Executable, Value } },
                         interpreter::{ ErrorWhat, Interpreter, InterpreterError,
                                        InterpreterResult },
                         text::location::Location } };


impl Interpreter
{
    pub fn default_prompt(&mut self) -> String
    {
        let cwd = self.evaluate_variable("$pwd").unwrap_or_else(|_|
            current_dir().unwrap_or_else(|_| ".".into()).display().to_string());
        if self.color_mode == TtyColorMode::TtyMonochrome
        { return format!("\n<shelly> [{}]\n", cwd); }
        format!("\n{} [{}]\n", Color::Yellow.bold().paint("<shelly>"), Color::Cyan.paint(cwd))
    }

    pub(crate) fn default_prompt_value() -> Value
    {
        let function = NativeFunction::new("<default_prompt>", NativeVisibility::Hidden,
            Rc::new(|interpreter, _, _|
                {
                    interpreter.last_result = Some(Value::from_string(
                        interpreter.default_prompt()));
                    Ok(())
                }));
        Value::String(function.name.into(), Executable::Native(function))
    }

    fn render_widgets(&mut self, location: &Location) -> InterpreterResult<String>
    {
        let Value::Array(widgets) = self.read_raw_variable("$widgets", location)? else
        {
            return Err(InterpreterError::new(
                location.clone(),
                ErrorWhat::InvalidOperand("Widgets require an array".into())
            ));
        };
        let widget_type = self.scope().types.native_types["WidgetFn"].id;
        let mut rendered = String::new();
        let (brackets, reset) = match self.color_mode
            {
                TtyColorMode::TtyTrueColor => ("\x1b[38;2;100;100;100m", "\x1b[39m"),
                TtyColorMode::Tty256 => ("\x1b[38;5;241m", "\x1b[39m"),
                TtyColorMode::TtyBasic => ("\x1b[90m", "\x1b[39m"),
                TtyColorMode::TtyMonochrome => ("", ""),
            };
        let contents = if self.color_mode == TtyColorMode::TtyMonochrome { String::new() }
        else
        {
            let Value::String(color, _) = self.read_raw_variable("$widget_color", location)? else
            {
                return Err(InterpreterError::new(
                    location.clone(),
                    ErrorWhat::InvalidOperand("Widget color must be a String".into())
                ));
            };
            // An empty setting uses the terminal's default text color, not the bracket grey.
            if color.is_empty() { reset.to_string() } else { color }
        };
        // Snapshot the callback list; changes made by widgets apply to the next prompt.
        for widget in widgets.iter()
        {
            let widget = self.scope().types.coerce(widget_type, widget.clone())
                .map_err(|message| InterpreterError::new(
                    location.clone(),
                    ErrorWhat::InvalidOperand(message)
                ))?;
            self.last_result = None;
            self.execute_value(location, widget, Vec::new())?;
            if self.halted { break; }
            let Value::String(text, _) = self.last_result.take().unwrap_or(Value::None) else
            {
                return Err(InterpreterError::new(
                    location.clone(),
                    ErrorWhat::InvalidOperand("Widget must return a String".into())
                ));
            };
            if text.is_empty() { continue; }
            rendered.push_str(brackets);
            rendered.push('[');
            rendered.push_str(&contents);
            rendered.push_str(&text);
            rendered.push_str(brackets);
            rendered.push(']');
            rendered.push_str(reset);
        }
        Ok(rendered)
    }

    fn prompt_home(&self, value: &Value) -> String
    {
        match value.underlying()
        {
            Value::Closure(closure) => closure.function.functions.borrow().scope.clone(),
            Value::Callable(callable) => self.prompt_home(&callable.target),
            Value::String(_, Executable::Function(function)) =>
                function.functions.borrow().scope.clone(),
            Value::String(_, Executable::Method(method)) => match &method.definition
                {
                    MethodDefinition::User(function) =>
                        function.functions.borrow().scope.clone(),
                    MethodDefinition::Builtin(_) => self.current_scope.clone(),
                },
            _ => self.current_scope.clone(),
        }
    }

    pub fn evaluate_prompt(&mut self, location: Location) -> InterpreterResult<Option<String>>
    {
        let previous_result = self.last_result.take();
        let result = (||
            {
                // Only return values contribute widget text, not printed stdout.
                let environment = self.exported_environment(&location)?;
                if self.halted { return Ok(None); }
                let previous_environment = self.widget_environment.replace(environment);
                let (rendered, _) = self.capture_stdout(|interpreter|
                    interpreter.render_widgets(&location));
                self.widget_environment = previous_environment;
                let rendered = rendered?;
                if self.halted { return Ok(None); }

                if self.variable_binding("$prompt").is_none() { return Ok(None); }
                let prompt_type = self.scope().types.native_types["Prompt"].id;
                let prompt = self.scope().types.coerce(prompt_type,
                    self.read_raw_variable("$prompt", &location)?)
                    .map_err(|message| InterpreterError::new(
                        location.clone(),
                        ErrorWhat::InvalidOperand(message)
                    ))?;
                let home = self.prompt_home(&prompt);
                let scope = self.scopes.get_mut(&home).expect("Prompt module must exist");
                let initial_scope = scope.variables.current_scope();
                scope.variables.push_scope();
                let _ = scope.variables.create("$rendered_widgets".into(), ScopedValue
                    {
                        value: Value::from_string(rendered),
                        type_id: scope.types.builtin_id("String"),
                        exported: ValueVisibility::Private,
                        reference: None,
                        dispatch_type: None,
                        environment_conversion: None,
                    });
                let binding = scope.variables.binding("$rendered_widgets").unwrap();
                let previous_widgets = self.prompt_widgets.replace(binding);
                let result = if prompt.is_callable()
                {
                    self.last_result = None;
                    // Printed output is discarded; only the returned String is prompt text.
                    let (result, _) = self.capture_stdout(|interpreter|
                        interpreter.execute_value(&location, prompt, Vec::new()));
                    result.map(|_| self.last_result.take().unwrap_or(Value::None).as_text())
                }
                else
                { self.interpolate_string(&location, &prompt.as_text()) };
                self.prompt_widgets = previous_widgets;
                self.scopes.get_mut(&home).unwrap().variables.reset_to_scope(initial_scope);
                result.map(Some)
            })();
        self.last_result = previous_result;
        result
    }
}
