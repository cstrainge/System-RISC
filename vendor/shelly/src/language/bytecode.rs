
use std::{ cell::RefCell, collections::{ HashMap, HashSet }, rc::Rc };

use crate::language::{ data::{ value::Value, types::TypeId }, text::location::Location };

pub enum Code
{
    Push,
    Try(Rc<CompiledTry>),
    Throw,
    Execute,
    // Evaluate a pipeline invocation without launching it; leave [callable, arguments].
    PreparePipelineCall,
    Pipeline(Rc<CompiledPipeline>),
    // Consume a symbol reference and publish it in the current module (`pub visible`).
    PublishSymbol,
    // Operand: [stream (1 stdout, 2 stderr, 3 both), variable, pending targets].
    BeginRedirect,
    EndRedirect,
    // Execute a command reference/name, or read bytes from a file path.
    RedirectSource,
    // Pop a command name; leave its result, or the unresolved string, in last_result.
    // Boolean(true) requires a command instead of falling back to text in a condition.
    TryExecute,
    // Execute last_result only when it is a string marked executable; preserve other values.
    // Boolean(true) rejects collections in a standalone variable statement whose value is
    // discarded.
    ExecuteIfExecutable,
    MakeExecutable,
    // Operand: [compiled function template, names of free bindings to capture].
    MakeClosure,
    ExitFunction,
    NewVariable,
    NewDefaultVariable,
    // Operand: [existing binding name, target type ID, visible conversion methods].
    RetypeVariable,
    // Widen and validate the stack top without consuming it, before replacing a binding.
    ValidateType,
    // Operand: [name, type ID or None, argument index, optional default value].
    BindParameter,
    // Operand: [name, type ID or None, first extra argument index].
    BindRestParameter,
    SetVariable,
    GetVariable,
    PushResult,
    PopResult,
    CheckResult,
    NewAlias,
    ExportVariable,
    GlobFiles,
    ExpandArray,
    // Stack operations: collect values, read an element, or update a variable's element.
    MakeArray,
    // Pop one array, validate its exact length, then push its elements in source order.
    UnpackArray,
    MakeHashMap,
    // Operand is [type ID, field indexes in evaluation order].
    MakeStruct,
    // Operand is [checked field index, literal name, or $variable name; visible method versions].
    // Consume a receiver reference; read data or invoke a method without explicit arguments.
    GetField,
    // Resolve and validate a dynamic String field name before an assignment's RHS.
    ResolveFieldName,
    // Receiver references use a separate VM stack and never become language values.
    ReferenceVariable,
    ReferenceValue,
    ReferenceField,
    ReferenceIndex,
    // Boolean operands enable/disable implicit calls; integer 1 enables data-field calls only.
    ReferenceGroup,
    // Like GetField, but preserve a bound method without invoking it.
    BindField,
    // Operand bits indicate a supplied start (1), end (2), and inclusive end (4).
    MakeRange,
    // Start captures a private receiver and the next_item method snapshot. Next
    // pushes one non-None item and sets a boolean result for the linked jump.
    StartIteration,
    NextIteration,
    // Pop a yielded value into a fresh scoped binding without assignment coercions.
    BindIteration,
    EndIteration,
    // Operand is [continue target, break target]: labels before linking, numeric
    // JumpTarget indexes afterwards. Enter pushes a loop frame; Exit pops it.
    EnterLoop,
    ExitLoop,
    Break,
    Continue,
    GetElement,
    SetElement,
    ExpandPath,
    InterpolateString,
    InterpolateGlob,
    EnterScope,
    ExitScope,
    MathAdd,
    MathSubtract,
    MathMultiply,
    MathDivide,
    MathModulo,
    CompareEqual,
    CompareNotEqual,
    CompareLess,
    CompareLessEqual,
    CompareGreater,
    CompareGreaterEqual,
    // Pop a pattern, compare against the retained subject, and set last_result.
    // A successful comparison also consumes the subject; failure keeps it for the next arm.
    MatchPattern,
    // Discard the stack top without treating an ExecResult as a command failure.
    Discard,
    MatchFail,
    // Convert last_result in place; these instructions do not touch the value stack.
    ToBoolean,
    // Operand: [resolved target TypeId, visible factory versions, source constraint/signature].
    ConvertType,
    BooleanNot,
    MathNegate,
    // Before linking: label IDs local to this code vector. After linking: direct
    // instruction indexes pointing at JumpTarget. Preserve last_result and the stack.
    // Unconditional jumps also form loop back edges.
    Jump,
    JumpIfFalse,
    JumpIfTrue,
    // A labeled landing point during compilation; linking removes its operand.
    JumpTarget,
}


pub struct Instruction
{
    pub location: Option<Location>,
    pub code: Code,
    pub operand: Option<Value>
}

pub struct CompiledCatch
{
    pub location: Location,
    pub stack: Option<String>,
    pub error: String,
    pub type_id: Option<TypeId>,
    pub code: Vec<Instruction>,
}

pub struct CompiledTry
{
    pub body: Vec<Instruction>,
    pub catches: Vec<CompiledCatch>,
    pub finally: Option<Vec<Instruction>>,
}

pub struct PipelineBoundary
{
    pub location: Location,
    pub decode: Option<String>,
    pub annotation: Option<String>,
    pub type_id: Option<TypeId>,
    pub conversions: Value,
}


pub struct PipelineRedirection
{
    pub location: Location,
    pub stream: i64,
    pub variable: bool,
    pub code: Vec<Instruction>,
}


pub struct PipelineStage
{
    pub location: Location,
    pub code: Vec<Instruction>,
    pub call: bool,
    pub command_word: bool,
    pub explicit_arguments: bool,
    pub input_used: bool,
    pub property: bool,
    pub destination: Option<String>,
    pub redirects: Vec<PipelineRedirection>,
    pub conversions: Value,
}


pub struct CompiledPipeline
{
    pub stages: Vec<PipelineStage>,
    pub boundaries: Vec<PipelineBoundary>,
    pub parents: Vec<usize>,
    pub primary_len: usize,
}


pub struct Function
{
    pub functions: FunctionBlockRef,
    // Type owning an extension, including receiver-free conversion factories.
    pub receiver_type: Option<TypeId>,
    // A typedef default yields to a scripted factory when module interfaces overlap.
    pub implied_conversion: bool,

    pub arguments: Vec<String>,
    pub argument_types: Vec<Option<TypeId>>,
    pub minimum_arguments: usize,
    pub variadic: bool,
    pub return_type: Option<TypeId>,
    pub code: Vec<Instruction>
}


#[derive(Clone)]
pub struct FunctionBlock
{
    pub scope: String,
    pub parent: Option<FunctionBlockRef>,
    pub function_name: Option<String>,
    pub protocol_owner: Option<TypeId>,
    pub declared_functions: HashSet<String>,
    pub builtins: Rc<HashSet<&'static str>>,
    pub functions: HashMap<String, FunctionRef>
}


pub type FunctionBlockRef = Rc<RefCell<FunctionBlock>>;

pub type FunctionRef = Rc<Function>;
