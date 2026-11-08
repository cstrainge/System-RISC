
use std::collections::{ BTreeMap, HashMap };

use crate::language::{ ast::{ AstExpression, AstPipeBoundary, AstPipeline },
                       parser::{ base_utils::{ Lookahead, try_expect_token },
                                 expressions::{ parse_pipe_boundary, parse_pipe_boundary_kind,
                                                parse_stage_before_block },
                                 results::{ ParseResult, ParserError, ParserErrorKind } },
                       text::{ layout::LayoutPosition, location::Location },
                       tokenizer::{ TokenBuffer, TokenKind } };


#[derive(Clone)]
struct Anchor
{
    stage: usize,
    location: Location,
    layout: LayoutPosition,
}


fn fault(pipeline: &mut AstPipeline, location: Location, layout: LayoutPosition,
         message: String, tabs: bool)
{
    if pipeline.layout_error.is_some() { return; }
    let hint = if tabs || layout.hard_tabs
        { " Hard tabs occur in this diagram; add or check '#tab-size N' in the header \
           to match your editor's tab size." } else { "" };
    pipeline.layout_error = Some((location, format!("{} Effective tab size: {}.{}",
        message, layout.tab_width, hint)));
}


// Keep the drawn paths, not just a list of earlier columns. A missing guide must
// never reconnect a branch to an unrelated pipe farther up the source.
pub(super) fn parse_branches(buffer: &mut TokenBuffer<'_, '_>,
                             stages: Vec<AstExpression>, boundaries: Vec<AstPipeBoundary>,
                             stop_at_block: bool, stop_at_colon: bool) -> ParseResult<AstPipeline>
{
    let primary_len = stages.len();
    let parents = (0..boundaries.len()).collect();
    let mut pipeline = AstPipeline
        { stages, boundaries, parents, primary_len, layout_error: None };
    let mut line = pipeline.stages.last().unwrap().location.line;
    let mut previous = BTreeMap::new();
    for (stage, boundary) in pipeline.boundaries.iter().enumerate()
    {
        if boundary.location.line == line
        {
            previous.insert(boundary.layout.column, Anchor { stage,
                location: boundary.location.clone(), layout: boundary.layout });
        }
    }
    let mut outstanding: HashMap<usize, Anchor> = HashMap::new();
    loop
    {
        let mut trial = Lookahead::new(buffer);
        if try_expect_token(trial.buffer, TokenKind::LineBreak)?.is_none() { break; }
        // Read through blank rows only to diagnose a later broken connection.
        // Its physical line number still exposes every missing guide segment.
        while try_expect_token(trial.buffer, TokenKind::LineBreak)?.is_some() {}
        let mut guides = Vec::new();
        while let Some(guide) = try_expect_token(trial.buffer, TokenKind::Pipe)?
        { guides.push(guide); }
        let boundary = parse_pipe_boundary_kind(trial.buffer, TokenKind::Branch)?;
        if guides.is_empty() && boundary.is_none() { break; }
        let row = boundary.as_ref().map(|item| item.location.line)
            .unwrap_or_else(|| guides[0].location.line);
        let tabs = previous.values().any(|item| item.layout.hard_tabs);
        let mut current = BTreeMap::new();
        for guide in &guides
        {
            let layout = trial.buffer.anchor(guide);
            if row == line + 1 && let Some(anchor) = previous.get(&layout.column)
            {
                outstanding.insert(anchor.stage, anchor.clone());
                current.insert(layout.column, anchor.clone());
            }
            else
            {
                fault(&mut pipeline, guide.location.clone(), layout,
                    format!("Vertical guide at column {} does not continue a parent pipe; \
                        the connection is broken or misaligned.", layout.column), tabs);
            }
        }
        if let Some(boundary) = boundary
        {
            let layout = boundary.layout;
            let parent = if row == line + 1 { previous.get(&layout.column) } else { None };
            let parent = match parent
            {
                Some(anchor) =>
                    {
                        outstanding.remove(&anchor.stage);
                        anchor.stage
                    },
                None =>
                    {
                        let eligible = previous.keys().map(usize::to_string)
                            .collect::<Vec<_>>().join(", ");
                        fault(&mut pipeline, boundary.location.clone(), layout,
                            format!("Branch connector at column {} does not align with a \
                                connected parent pipe. Missing vertical guides or misalignment; \
                                connected columns on the preceding row: [{}].",
                                layout.column, eligible), tabs);
                        0
                    },
            };
            if guides.iter().any(|guide| trial.buffer.anchor(guide).column >= layout.column)
            {
                fault(&mut pipeline, boundary.location.clone(), layout,
                    "Vertical guides must remain to the left of their branch connector".into(),
                    tabs);
            }
            let stage = parse_stage_before_block(trial.buffer, stop_at_block, stop_at_colon)?
                .ok_or_else(|| ParserError { location: Some(boundary.location.clone()),
                    kind: ParserErrorKind::ExpectedExpression })?;
            pipeline.parents.push(parent);
            pipeline.boundaries.push(boundary);
            pipeline.stages.push(stage);
            while let Some(boundary) = parse_pipe_boundary(trial.buffer)?
            {
                let stage = pipeline.stages.len() - 1;
                current.insert(boundary.layout.column, Anchor { stage,
                    location: boundary.location.clone(), layout: boundary.layout });
                let next = parse_stage_before_block(trial.buffer, stop_at_block, stop_at_colon)?
                    .ok_or_else(|| ParserError { location: Some(boundary.location.clone()),
                        kind: ParserErrorKind::ExpectedExpression })?;
                pipeline.parents.push(stage);
                pipeline.boundaries.push(boundary);
                pipeline.stages.push(next);
            }
        }
        if pipeline.stages.len() > 128
        {
            return Err(ParserError { location: Some(pipeline.stages[0].location.clone()),
                kind: ParserErrorKind::InvalidPipeline("Pipeline exceeds 128 stages".into()) });
        }
        trial.commit();
        previous = current;
        line = row;
    }
    if let Some(anchor) = outstanding.values().min_by_key(|item| item.stage)
    {
        fault(&mut pipeline, anchor.location.clone(), anchor.layout,
            "Dangling vertical guide: every drawn connection must end in a branch connector".into(),
            anchor.layout.hard_tabs);
    }
    if pipeline.primary_len != pipeline.stages.len()
    {
        let mut newline = Lookahead::new(buffer);
        if try_expect_token(newline.buffer, TokenKind::LineBreak)?.is_some()
        {
            let close =
                {
                    let peek = Lookahead::new(newline.buffer);
                    peek.buffer.next()?.map(|token| token.kind)
                };
            if matches!(close, Some(TokenKind::ParenClose | TokenKind::SquareClose))
            { newline.commit(); }
        }
    }
    if let Some((location, message)) = &pipeline.layout_error
    { buffer.layout_diagnostic(location, message); }
    Ok(pipeline)
}
