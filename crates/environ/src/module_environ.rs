use crate::module::{
    FuncRefIndex, Initializer, MemoryInitialization, MemoryInitializer, MemoryPlan, Module,
    ModuleType, TablePlan, TableSegment,
};
use crate::{
    DataIndex, DefinedFuncIndex, ElemIndex, EntityIndex, EntityType, FuncIndex, GlobalIndex,
    GlobalInit, MemoryIndex, ModuleTypesBuilder, PrimaryMap, SignatureIndex, TableIndex,
    TableInitialValue, Tunables, TypeConvert, TypeIndex, Unsigned, WasmError, WasmHeapType,
    WasmResult, WasmType, WasmparserTypeConverter,
};
use cranelift_entity::packed_option::ReservedValue;
use std::borrow::Cow;
use std::collections::HashMap;
use std::convert::TryFrom;
use std::path::PathBuf;
use std::sync::Arc;
use wasmparser::types::{CoreTypeId, Types};
use wasmparser::{
    CompositeType, CustomSectionReader, DataKind, ElementItems, ElementKind, Encoding,
    ExternalKind, FuncToValidate, FunctionBody, NameSectionReader, Naming, Operator, Parser,
    Payload, TypeRef, Validator, ValidatorResources,
};

/// Object containing the standalone environment information.
pub struct ModuleEnvironment<'a, 'data> {
    /// The current module being translated
    result: ModuleTranslation<'data>,

    /// Intern'd types for this entire translation, shared by all modules.
    types: &'a mut ModuleTypesBuilder,

    // Various bits and pieces of configuration
    validator: &'a mut Validator,
    tunables: &'a Tunables,
}

/// The result of translating via `ModuleEnvironment`. Function bodies are not
/// yet translated, and data initializers have not yet been copied out of the
/// original buffer.
#[derive(Default)]
pub struct ModuleTranslation<'data> {
    /// Module information.
    pub module: Module,

    /// The input wasm binary.
    ///
    /// This can be useful, for example, when modules are parsed from a
    /// component and the embedder wants access to the raw wasm modules
    /// themselves.
    pub wasm: &'data [u8],

    /// References to the function bodies.
    pub function_body_inputs: PrimaryMap<DefinedFuncIndex, FunctionBodyData<'data>>,

    /// A list of type signatures which are considered exported from this
    /// module, or those that can possibly be called. This list is sorted, and
    /// trampolines for each of these signatures are required.
    pub exported_signatures: Vec<SignatureIndex>,

    /// DWARF debug information, if enabled, parsed from the module.
    pub debuginfo: DebugInfoData<'data>,

    /// Set if debuginfo was found but it was not parsed due to `Tunables`
    /// configuration.
    pub has_unparsed_debuginfo: bool,

    /// List of data segments found in this module which should be concatenated
    /// together for the final compiled artifact.
    ///
    /// These data segments, when concatenated, are indexed by the
    /// `MemoryInitializer` type.
    pub data: Vec<Cow<'data, [u8]>>,

    /// The desired alignment of `data` in the final data section of the object
    /// file that we'll emit.
    ///
    /// Note that this is 1 by default but `MemoryInitialization::Static` might
    /// switch this to a higher alignment to facilitate mmap-ing data from
    /// an object file into a linear memory.
    pub data_align: Option<u64>,

    /// Total size of all data pushed onto `data` so far.
    total_data: u32,

    /// List of passive element segments found in this module which will get
    /// concatenated for the final artifact.
    pub passive_data: Vec<&'data [u8]>,

    /// Total size of all passive data pushed into `passive_data` so far.
    total_passive_data: u32,

    /// When we're parsing the code section this will be incremented so we know
    /// which function is currently being defined.
    code_index: u32,

    /// The type information of the current module made available at the end of the
    /// validation process.
    types: Option<Types>,
}

impl<'data> ModuleTranslation<'data> {
    /// Returns a reference to the type information of the current module.
    pub fn get_types(&self) -> &Types {
        self.types
            .as_ref()
            .expect("module type information to be available")
    }
}

/// Contains function data: byte code and its offset in the module.
pub struct FunctionBodyData<'a> {
    /// The body of the function, containing code and locals.
    pub body: FunctionBody<'a>,
    /// Validator for the function body
    pub validator: FuncToValidate<ValidatorResources>,
}

#[derive(Debug, Default)]
#[allow(missing_docs)]
pub struct DebugInfoData<'a> {
    pub dwarf: Dwarf<'a>,
    pub name_section: NameSection<'a>,
    pub wasm_file: WasmFileInfo,
    debug_loc: gimli::DebugLoc<Reader<'a>>,
    debug_loclists: gimli::DebugLocLists<Reader<'a>>,
    pub debug_ranges: gimli::DebugRanges<Reader<'a>>,
    pub debug_rnglists: gimli::DebugRngLists<Reader<'a>>,
}

#[allow(missing_docs)]
pub type Dwarf<'input> = gimli::Dwarf<Reader<'input>>;

type Reader<'input> = gimli::EndianSlice<'input, gimli::LittleEndian>;

#[derive(Debug, Default)]
#[allow(missing_docs)]
pub struct NameSection<'a> {
    pub module_name: Option<&'a str>,
    pub func_names: HashMap<FuncIndex, &'a str>,
    pub locals_names: HashMap<FuncIndex, HashMap<u32, &'a str>>,
}

#[derive(Debug, Default)]
#[allow(missing_docs)]
pub struct WasmFileInfo {
    pub path: Option<PathBuf>,
    pub code_section_offset: u64,
    pub imported_func_count: u32,
    pub funcs: Vec<FunctionMetadata>,
}

#[derive(Debug)]
#[allow(missing_docs)]
pub struct FunctionMetadata {
    pub params: Box<[WasmType]>,
    pub locals: Box<[(u32, WasmType)]>,
}

impl<'a, 'data> ModuleEnvironment<'a, 'data> {
    /// Allocates the environment data structures.
    pub fn new(
        tunables: &'a Tunables,
        validator: &'a mut Validator,
        types: &'a mut ModuleTypesBuilder,
    ) -> Self {
        Self {
            result: ModuleTranslation::default(),
            types,
            tunables,
            validator,
        }
    }

    /// Translate a wasm module using this environment.
    ///
    /// This function will translate the `data` provided with `parser`,
    /// validating everything along the way with this environment's validator.
    ///
    /// The result of translation, [`ModuleTranslation`], contains everything
    /// necessary to compile functions afterwards as well as learn type
    /// information about the module at runtime.
    pub fn translate(
        mut self,
        parser: Parser,
        data: &'data [u8],
    ) -> WasmResult<ModuleTranslation<'data>> {
        self.result.wasm = data;

        for payload in parser.parse_all(data) {
            self.translate_payload(payload?)?;
        }

        Ok(self.result)
    }

    fn translate_payload(&mut self, payload: Payload<'data>) -> WasmResult<()> {
        match payload {
            Payload::Version {
                num,
                encoding,
                range,
            } => {
                self.validator.version(num, encoding, &range)?;
                match encoding {
                    Encoding::Module => {}
                    Encoding::Component => {
                        return Err(WasmError::Unsupported(format!("component model")));
                    }
                }
            }

            Payload::End(offset) => {
                self.result.types = Some(self.validator.end(offset)?);

                // With the `escaped_funcs` set of functions finished
                // we can calculate the set of signatures that are exported as
                // the set of exported functions' signatures.
                self.result.exported_signatures = self
                    .result
                    .module
                    .functions
                    .iter()
                    .filter_map(|(_, func)| {
                        if func.is_escaping() {
                            Some(func.signature)
                        } else {
                            None
                        }
                    })
                    .collect();
                self.result.exported_signatures.sort_unstable();
                self.result.exported_signatures.dedup();
            }

            Payload::TypeSection(types) => {
                self.validator.type_section(&types)?;
                let num = usize::try_from(types.count()).unwrap();
                self.result.module.types.reserve(num);
                self.types.reserve_wasm_signatures(num);

                for i in 0..types.count() {
                    let types = self.validator.types(0).unwrap();
                    let ty = types.core_type_at(i);
                    self.declare_type(ty.unwrap_sub())?;
                }
            }

            Payload::ImportSection(imports) => {
                self.validator.import_section(&imports)?;

                let cnt = usize::try_from(imports.count()).unwrap();
                self.result.module.initializers.reserve(cnt);

                for entry in imports {
                    let import = entry?;
                    let ty = match import.ty {
                        TypeRef::Func(index) => {
                            let index = TypeIndex::from_u32(index);
                            let sig_index = self.result.module.types[index].unwrap_function();
                            self.result.module.num_imported_funcs += 1;
                            self.result.debuginfo.wasm_file.imported_func_count += 1;
                            EntityType::Function(sig_index)
                        }
                        TypeRef::Memory(ty) => {
                            self.result.module.num_imported_memories += 1;
                            EntityType::Memory(ty.into())
                        }
                        TypeRef::Global(ty) => {
                            self.result.module.num_imported_globals += 1;
                            EntityType::Global(self.convert_global_type(&ty))
                        }
                        TypeRef::Table(ty) => {
                            self.result.module.num_imported_tables += 1;
                            EntityType::Table(self.convert_table_type(&ty))
                        }

                        // doesn't get past validation
                        TypeRef::Tag(_) => unreachable!(),
                    };
                    self.declare_import(import.module, import.name, ty);
                }
            }

            Payload::FunctionSection(functions) => {
                self.validator.function_section(&functions)?;

                let cnt = usize::try_from(functions.count()).unwrap();
                self.result.module.functions.reserve_exact(cnt);

                for entry in functions {
                    let sigindex = entry?;
                    let ty = TypeIndex::from_u32(sigindex);
                    let sig_index = self.result.module.types[ty].unwrap_function();
                    self.result.module.push_function(sig_index);
                }
            }

            Payload::TableSection(tables) => {
                self.validator.table_section(&tables)?;
                let cnt = usize::try_from(tables.count()).unwrap();
                self.result.module.table_plans.reserve_exact(cnt);

                for entry in tables {
                    let wasmparser::Table { ty, init } = entry?;
                    let table = self.convert_table_type(&ty);
                    let plan = TablePlan::for_table(table, &self.tunables);
                    self.result.module.table_plans.push(plan);
                    let init = match init {
                        wasmparser::TableInit::RefNull => TableInitialValue::Null {
                            precomputed: Vec::new(),
                        },
                        wasmparser::TableInit::Expr(cexpr) => {
                            let mut init_expr_reader = cexpr.get_binary_reader();
                            match init_expr_reader.read_operator()? {
                                Operator::RefNull { hty: _ } => TableInitialValue::Null {
                                    precomputed: Vec::new(),
                                },
                                Operator::RefFunc { function_index } => {
                                    let index = FuncIndex::from_u32(function_index);
                                    self.flag_func_escaped(index);
                                    TableInitialValue::FuncRef(index)
                                }
                                s => {
                                    return Err(WasmError::Unsupported(format!(
                                        "unsupported init expr in table section: {:?}",
                                        s
                                    )));
                                }
                            }
                        }
                    };
                    self.result
                        .module
                        .table_initialization
                        .initial_values
                        .push(init);
                }
            }

            Payload::MemorySection(memories) => {
                self.validator.memory_section(&memories)?;

                let cnt = usize::try_from(memories.count()).unwrap();
                self.result.module.memory_plans.reserve_exact(cnt);

                for entry in memories {
                    let memory = entry?;
                    let plan = MemoryPlan::for_memory(memory.into(), &self.tunables);
                    self.result.module.memory_plans.push(plan);
                }
            }

            Payload::TagSection(tags) => {
                self.validator.tag_section(&tags)?;

                // This feature isn't enabled at this time, so we should
                // never get here.
                unreachable!();
            }

            Payload::GlobalSection(globals) => {
                self.validator.global_section(&globals)?;

                let cnt = usize::try_from(globals.count()).unwrap();
                self.result.module.globals.reserve_exact(cnt);

                for entry in globals {
                    let wasmparser::Global { ty, init_expr } = entry?;
                    let mut init_expr_reader = init_expr.get_binary_reader();
                    let initializer = match init_expr_reader.read_operator()? {
                        Operator::I32Const { value } => GlobalInit::I32Const(value),
                        Operator::I64Const { value } => GlobalInit::I64Const(value),
                        Operator::F32Const { value } => GlobalInit::F32Const(value.bits()),
                        Operator::F64Const { value } => GlobalInit::F64Const(value.bits()),
                        Operator::V128Const { value } => {
                            GlobalInit::V128Const(u128::from_le_bytes(*value.bytes()))
                        }
                        Operator::RefNull { hty: _ } => GlobalInit::RefNullConst,
                        Operator::RefFunc { function_index } => {
                            let index = FuncIndex::from_u32(function_index);
                            self.flag_func_escaped(index);
                            GlobalInit::RefFunc(index)
                        }
                        Operator::GlobalGet { global_index } => {
                            GlobalInit::GetGlobal(GlobalIndex::from_u32(global_index))
                        }
                        s => {
                            return Err(WasmError::Unsupported(format!(
                                "unsupported init expr in global section: {:?}",
                                s
                            )));
                        }
                    };
                    let ty = self.convert_global_type(&ty);
                    self.result.module.globals.push(ty);
                    self.result.module.global_initializers.push(initializer);
                }
            }

            Payload::ExportSection(exports) => {
                self.validator.export_section(&exports)?;

                let cnt = usize::try_from(exports.count()).unwrap();
                self.result.module.exports.reserve(cnt);

                for entry in exports {
                    let wasmparser::Export { name, kind, index } = entry?;
                    let entity = match kind {
                        ExternalKind::Func => {
                            let index = FuncIndex::from_u32(index);
                            self.flag_func_escaped(index);
                            EntityIndex::Function(index)
                        }
                        ExternalKind::Table => EntityIndex::Table(TableIndex::from_u32(index)),
                        ExternalKind::Memory => EntityIndex::Memory(MemoryIndex::from_u32(index)),
                        ExternalKind::Global => EntityIndex::Global(GlobalIndex::from_u32(index)),

                        // this never gets past validation
                        ExternalKind::Tag => unreachable!(),
                    };
                    self.result
                        .module
                        .exports
                        .insert(String::from(name), entity);
                }
            }

            Payload::StartSection { func, range } => {
                self.validator.start_section(func, &range)?;

                let func_index = FuncIndex::from_u32(func);
                self.flag_func_escaped(func_index);
                debug_assert!(self.result.module.start_func.is_none());
                self.result.module.start_func = Some(func_index);
            }

            Payload::ElementSection(elements) => {
                self.validator.element_section(&elements)?;

                for (index, entry) in elements.into_iter().enumerate() {
                    let wasmparser::Element {
                        kind,
                        items,
                        range: _,
                    } = entry?;

                    // Build up a list of `FuncIndex` corresponding to all the
                    // entries listed in this segment. Note that it's not
                    // possible to create anything other than a `ref.null
                    // extern` for externref segments, so those just get
                    // translated to the reserved value of `FuncIndex`.
                    let mut elements = Vec::new();
                    match items {
                        ElementItems::Functions(funcs) => {
                            elements.reserve(usize::try_from(funcs.count()).unwrap());
                            for func in funcs {
                                let func = FuncIndex::from_u32(func?);
                                self.flag_func_escaped(func);
                                elements.push(func);
                            }
                        }
                        ElementItems::Expressions(_ty, funcs) => {
                            elements.reserve(usize::try_from(funcs.count()).unwrap());
                            for func in funcs {
                                let func = match func?.get_binary_reader().read_operator()? {
                                    Operator::RefNull { .. } => FuncIndex::reserved_value(),
                                    Operator::RefFunc { function_index } => {
                                        let func = FuncIndex::from_u32(function_index);
                                        self.flag_func_escaped(func);
                                        func
                                    }
                                    s => {
                                        return Err(WasmError::Unsupported(format!(
                                            "unsupported init expr in element section: {:?}",
                                            s
                                        )));
                                    }
                                };
                                elements.push(func);
                            }
                        }
                    }

                    match kind {
                        ElementKind::Active {
                            table_index,
                            offset_expr,
                        } => {
                            let table_index = TableIndex::from_u32(table_index.unwrap_or(0));
                            let mut offset_expr_reader = offset_expr.get_binary_reader();
                            let (base, offset) = match offset_expr_reader.read_operator()? {
                                Operator::I32Const { value } => (None, value.unsigned()),
                                Operator::GlobalGet { global_index } => {
                                    (Some(GlobalIndex::from_u32(global_index)), 0)
                                }
                                ref s => {
                                    return Err(WasmError::Unsupported(format!(
                                        "unsupported init expr in element section: {:?}",
                                        s
                                    )));
                                }
                            };

                            self.result
                                .module
                                .table_initialization
                                .segments
                                .push(TableSegment {
                                    table_index,
                                    base,
                                    offset,
                                    elements: elements.into(),
                                });
                        }

                        ElementKind::Passive => {
                            let elem_index = ElemIndex::from_u32(index as u32);
                            let index = self.result.module.passive_elements.len();
                            self.result.module.passive_elements.push(elements.into());
                            self.result
                                .module
                                .passive_elements_map
                                .insert(elem_index, index);
                        }

                        ElementKind::Declared => {}
                    }
                }
            }

            Payload::CodeSectionStart { count, range, .. } => {
                self.validator.code_section_start(count, &range)?;
                let cnt = usize::try_from(count).unwrap();
                self.result.function_body_inputs.reserve_exact(cnt);
                self.result.debuginfo.wasm_file.code_section_offset = range.start as u64;
            }

            Payload::CodeSectionEntry(mut body) => {
                let validator = self.validator.code_section_entry(&body)?;
                let func_index =
                    self.result.code_index + self.result.module.num_imported_funcs as u32;
                let func_index = FuncIndex::from_u32(func_index);

                if self.tunables.generate_native_debuginfo {
                    let sig_index = self.result.module.functions[func_index].signature;
                    let sig = &self.types[sig_index];
                    let mut locals = Vec::new();
                    for pair in body.get_locals_reader()? {
                        let (cnt, ty) = pair?;
                        let ty = self.convert_valtype(ty);
                        locals.push((cnt, ty));
                    }
                    self.result
                        .debuginfo
                        .wasm_file
                        .funcs
                        .push(FunctionMetadata {
                            locals: locals.into_boxed_slice(),
                            params: sig.params().into(),
                        });
                }
                body.allow_memarg64(self.validator.features().memory64);
                self.result
                    .function_body_inputs
                    .push(FunctionBodyData { validator, body });
                self.result.code_index += 1;
            }

            Payload::DataSection(data) => {
                self.validator.data_section(&data)?;

                let initializers = match &mut self.result.module.memory_initialization {
                    MemoryInitialization::Segmented(i) => i,
                    _ => unreachable!(),
                };

                let cnt = usize::try_from(data.count()).unwrap();
                initializers.reserve_exact(cnt);
                self.result.data.reserve_exact(cnt);

                for (index, entry) in data.into_iter().enumerate() {
                    let wasmparser::Data {
                        kind,
                        data,
                        range: _,
                    } = entry?;
                    let mk_range = |total: &mut u32| -> Result<_, WasmError> {
                        let range = u32::try_from(data.len())
                            .ok()
                            .and_then(|size| {
                                let start = *total;
                                let end = start.checked_add(size)?;
                                Some(start..end)
                            })
                            .ok_or_else(|| {
                                WasmError::Unsupported(format!(
                                    "more than 4 gigabytes of data in wasm module",
                                ))
                            })?;
                        *total += range.end - range.start;
                        Ok(range)
                    };
                    match kind {
                        DataKind::Active {
                            memory_index,
                            offset_expr,
                        } => {
                            let range = mk_range(&mut self.result.total_data)?;
                            let memory_index = MemoryIndex::from_u32(memory_index);
                            let mut offset_expr_reader = offset_expr.get_binary_reader();
                            let (base, offset) = match offset_expr_reader.read_operator()? {
                                Operator::I32Const { value } => (None, value.unsigned().into()),
                                Operator::I64Const { value } => (None, value.unsigned()),
                                Operator::GlobalGet { global_index } => {
                                    (Some(GlobalIndex::from_u32(global_index)), 0)
                                }
                                s => {
                                    return Err(WasmError::Unsupported(format!(
                                        "unsupported init expr in data section: {:?}",
                                        s
                                    )));
                                }
                            };

                            initializers.push(MemoryInitializer {
                                memory_index,
                                base,
                                offset,
                                data: range,
                            });
                            self.result.data.push(data.into());
                        }
                        DataKind::Passive => {
                            let data_index = DataIndex::from_u32(index as u32);
                            let range = mk_range(&mut self.result.total_passive_data)?;
                            self.result.passive_data.push(data);
                            self.result
                                .module
                                .passive_data_map
                                .insert(data_index, range);
                        }
                    }
                }
            }

            Payload::DataCountSection { count, range } => {
                self.validator.data_count_section(count, &range)?;

                // Note: the count passed in here is the *total* segment count
                // There is no way to reserve for just the passive segments as
                // they are discovered when iterating the data section entries
                // Given that the total segment count might be much larger than
                // the passive count, do not reserve anything here.
            }

            Payload::CustomSection(s) if s.name() == "name" => {
                let result = self.name_section(NameSectionReader::new(s.data(), s.data_offset()));
                if let Err(e) = result {
                    log::warn!("failed to parse name section {:?}", e);
                }
            }

            Payload::CustomSection(s)
                if s.name() == "webidl-bindings" || s.name() == "wasm-interface-types" =>
            {
                return Err(WasmError::Unsupported(
                    "\
Support for interface types has temporarily been removed from `wasmtime`.

For more information about this temporary change you can read on the issue online:

    https://github.com/bytecodealliance/wasmtime/issues/1271

and for re-adding support for interface types you can see this issue:

    https://github.com/bytecodealliance/wasmtime/issues/677
"
                    .to_string(),
                ))
            }

            Payload::CustomSection(s) => {
                self.register_dwarf_section(&s);
            }

            // It's expected that validation will probably reject other
            // payloads such as `UnknownSection` or those related to the
            // component model. If, however, something gets past validation then
            // that's a bug in Wasmtime as we forgot to implement something.
            other => {
                self.validator.payload(&other)?;
                panic!("unimplemented section in wasm file {:?}", other);
            }
        }
        Ok(())
    }

    fn register_dwarf_section(&mut self, section: &CustomSectionReader<'data>) {
        let name = section.name();
        if !name.starts_with(".debug_") {
            return;
        }
        if !self.tunables.generate_native_debuginfo && !self.tunables.parse_wasm_debuginfo {
            self.result.has_unparsed_debuginfo = true;
            return;
        }
        let info = &mut self.result.debuginfo;
        let dwarf = &mut info.dwarf;
        let endian = gimli::LittleEndian;
        let data = section.data();
        let slice = gimli::EndianSlice::new(data, endian);

        match name {
            // `gimli::Dwarf` fields.
            ".debug_abbrev" => dwarf.debug_abbrev = gimli::DebugAbbrev::new(data, endian),
            ".debug_addr" => dwarf.debug_addr = gimli::DebugAddr::from(slice),
            ".debug_info" => dwarf.debug_info = gimli::DebugInfo::new(data, endian),
            ".debug_line" => dwarf.debug_line = gimli::DebugLine::new(data, endian),
            ".debug_line_str" => dwarf.debug_line_str = gimli::DebugLineStr::from(slice),
            ".debug_str" => dwarf.debug_str = gimli::DebugStr::new(data, endian),
            ".debug_str_offsets" => dwarf.debug_str_offsets = gimli::DebugStrOffsets::from(slice),
            ".debug_str_sup" => {
                let mut dwarf_sup: Dwarf<'data> = Default::default();
                dwarf_sup.debug_str = gimli::DebugStr::from(slice);
                dwarf.sup = Some(Arc::new(dwarf_sup));
            }
            ".debug_types" => dwarf.debug_types = gimli::DebugTypes::from(slice),

            // Additional fields.
            ".debug_loc" => info.debug_loc = gimli::DebugLoc::from(slice),
            ".debug_loclists" => info.debug_loclists = gimli::DebugLocLists::from(slice),
            ".debug_ranges" => info.debug_ranges = gimli::DebugRanges::new(data, endian),
            ".debug_rnglists" => info.debug_rnglists = gimli::DebugRngLists::new(data, endian),

            // We don't use these at the moment.
            ".debug_aranges" | ".debug_pubnames" | ".debug_pubtypes" => return,

            other => {
                log::warn!("unknown debug section `{}`", other);
                return;
            }
        }

        dwarf.ranges = gimli::RangeLists::new(info.debug_ranges, info.debug_rnglists);
        dwarf.locations = gimli::LocationLists::new(info.debug_loc, info.debug_loclists);
    }

    /// Declares a new import with the `module` and `field` names, importing the
    /// `ty` specified.
    ///
    /// Note that this method is somewhat tricky due to the implementation of
    /// the module linking proposal. In the module linking proposal two-level
    /// imports are recast as single-level imports of instances. That recasting
    /// happens here by recording an import of an instance for the first time
    /// we see a two-level import.
    ///
    /// When the module linking proposal is disabled, however, disregard this
    /// logic and instead work directly with two-level imports since no
    /// instances are defined.
    fn declare_import(&mut self, module: &'data str, field: &'data str, ty: EntityType) {
        let index = self.push_type(ty);
        self.result.module.initializers.push(Initializer::Import {
            name: module.to_owned(),
            field: field.to_owned(),
            index,
        });
    }

    fn push_type(&mut self, ty: EntityType) -> EntityIndex {
        match ty {
            EntityType::Function(ty) => EntityIndex::Function(self.result.module.push_function(ty)),
            EntityType::Table(ty) => {
                let plan = TablePlan::for_table(ty, &self.tunables);
                EntityIndex::Table(self.result.module.table_plans.push(plan))
            }
            EntityType::Memory(ty) => {
                let plan = MemoryPlan::for_memory(ty, &self.tunables);
                EntityIndex::Memory(self.result.module.memory_plans.push(plan))
            }
            EntityType::Global(ty) => EntityIndex::Global(self.result.module.globals.push(ty)),
            EntityType::Tag(_) => unimplemented!(),
        }
    }

    fn flag_func_escaped(&mut self, func: FuncIndex) {
        let ty = &mut self.result.module.functions[func];
        // If this was already assigned a funcref index no need to re-assign it.
        if ty.is_escaping() {
            return;
        }
        let index = self.result.module.num_escaped_funcs as u32;
        ty.func_ref = FuncRefIndex::from_u32(index);
        self.result.module.num_escaped_funcs += 1;
    }

    fn declare_type(&mut self, id: CoreTypeId) -> WasmResult<()> {
        let types = self.validator.types(0).unwrap();
        let ty = &types[id];
        assert!(ty.is_final);
        assert!(ty.supertype_idx.is_none());
        match &ty.composite_type {
            CompositeType::Func(ty) => {
                let wasm = self.convert_func_type(ty);
                let sig_index = self.types.wasm_func_type(id, wasm);
                self.result
                    .module
                    .types
                    .push(ModuleType::Function(sig_index));
            }
            CompositeType::Array(_) | CompositeType::Struct(_) => unimplemented!(),
        }
        Ok(())
    }

    /// Parses the Name section of the wasm module.
    fn name_section(&mut self, names: NameSectionReader<'data>) -> WasmResult<()> {
        for subsection in names {
            match subsection? {
                wasmparser::Name::Function(names) => {
                    for name in names {
                        let Naming { index, name } = name?;
                        // Skip this naming if it's naming a function that
                        // doesn't actually exist.
                        if (index as usize) >= self.result.module.functions.len() {
                            continue;
                        }

                        // Store the name unconditionally, regardless of
                        // whether we're parsing debuginfo, since function
                        // names are almost always present in the
                        // final compilation artifact.
                        let index = FuncIndex::from_u32(index);
                        self.result
                            .debuginfo
                            .name_section
                            .func_names
                            .insert(index, name);
                    }
                }
                wasmparser::Name::Module { name, .. } => {
                    self.result.module.name = Some(name.to_string());
                    if self.tunables.generate_native_debuginfo {
                        self.result.debuginfo.name_section.module_name = Some(name);
                    }
                }
                wasmparser::Name::Local(reader) => {
                    if !self.tunables.generate_native_debuginfo {
                        continue;
                    }
                    for f in reader {
                        let f = f?;
                        // Skip this naming if it's naming a function that
                        // doesn't actually exist.
                        if (f.index as usize) >= self.result.module.functions.len() {
                            continue;
                        }
                        for name in f.names {
                            let Naming { index, name } = name?;

                            self.result
                                .debuginfo
                                .name_section
                                .locals_names
                                .entry(FuncIndex::from_u32(f.index))
                                .or_insert(HashMap::new())
                                .insert(index, name);
                        }
                    }
                }
                wasmparser::Name::Label(_)
                | wasmparser::Name::Type(_)
                | wasmparser::Name::Table(_)
                | wasmparser::Name::Global(_)
                | wasmparser::Name::Memory(_)
                | wasmparser::Name::Element(_)
                | wasmparser::Name::Data(_)
                | wasmparser::Name::Tag(_)
                | wasmparser::Name::Unknown { .. } => {}
            }
        }
        Ok(())
    }
}

impl TypeConvert for ModuleEnvironment<'_, '_> {
    fn lookup_heap_type(&self, index: wasmparser::UnpackedIndex) -> WasmHeapType {
        WasmparserTypeConverter {
            types: &self.types,
            module: &self.result.module,
        }
        .lookup_heap_type(index)
    }
}
