#![feature(entry_insert)]
#![feature(let_chains)]
#![feature(slice_as_chunks)]
#![feature(read_buf)]
#![feature(map_try_insert)]
#![feature(lazy_cell)]
#![feature(exit_status_error)]

use brocolib::{global_metadata::TypeDefinitionIndex, runtime_metadata::TypeData};
use color_eyre::{eyre::Context, Result, Section};
use generate::{config::GenerationConfig, metadata::Metadata};
use itertools::Itertools;
extern crate pretty_env_logger;
use filesize::PathExt;
use include_dir::{include_dir, Dir};
use json::json_gen::{make_json, make_json_folder};
use log::{error, info, trace, warn};
use rayon::prelude::*;
use walkdir::DirEntry;

use std::{fs, path::PathBuf, process::Command, sync::LazyLock, time};

use clap::{Parser, Subcommand};

use crate::{
    generate::{
        context_collection::CppContextCollection, cpp_type_tag::CppTypeTag,
        cs_context_collection::CsContextCollection, members::CppMember,
    },
    handlers::{comment_omit::remove_coments, object, unity, value_type},
};
mod data;
mod generate;
mod handlers;
mod helpers;
mod json;

#[derive(Parser)]
#[clap(author, version, about, long_about = None)]
struct Cli {
    /// The global-metadata.dat file to use
    #[clap(short, long, value_parser, value_name = "FILE")]
    metadata: PathBuf,

    /// The libil2cpp.so file to use
    #[clap(short, long, value_parser, value_name = "FILE")]
    libil2cpp: PathBuf,

    /// The path to generated json file
    #[clap(short, long, value_parser, value_name = "FILE")]
    json: Option<PathBuf>,

    /// The path to the folder for the generated json files
    #[clap(long, value_parser, value_name = "FILE")]
    multi_json: Option<PathBuf>,

    /// Whether to format with clang-format
    #[clap(short, long)]
    format: bool,
    /// Whether to format with clang-format
    #[clap(short, long)]
    remove_verbose_comments: bool,

    /// Whether to generate generic method specializations
    #[clap(short, long)]
    gen_generic_methods_specializations: bool,

    #[clap(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {}

pub static STATIC_CONFIG: LazyLock<GenerationConfig> = LazyLock::new(|| GenerationConfig {
    header_path: PathBuf::from("./codegen/include"),
    source_path: PathBuf::from("./codegen/src"),
    dst_internals_path: PathBuf::from("./codegen/include/cordl_internals"),
    dst_header_internals_file: PathBuf::from(
        "./codegen/include/cordl_internals/cordl_internals.hpp",
    ),
    use_anonymous_namespace: false,
});

static INTERNALS_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/cordl_internals");

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    let cli: Cli = Cli::parse();
    pretty_env_logger::formatted_builder()
        .filter_level(log::LevelFilter::Trace)
        .parse_default_env()
        .init();
    if !cli.format {
        info!("Add --format/-f to format with clang-format at end")
    }

    if STATIC_CONFIG.header_path.exists() {
        std::fs::remove_dir_all(&STATIC_CONFIG.header_path)?;
    }
    std::fs::create_dir_all(&STATIC_CONFIG.header_path)?;

    info!(
        "Copying config to codegen folder {:?}",
        STATIC_CONFIG.dst_internals_path
    );

    std::fs::create_dir_all(&STATIC_CONFIG.dst_internals_path)?;

    // extract contents of the cordl internals folder into destination
    INTERNALS_DIR.extract(&STATIC_CONFIG.dst_internals_path)?;

    let global_metadata_data = fs::read(cli.metadata).context("il2cpp metadata")?;
    let elf_data = fs::read(cli.libil2cpp).context("libil2cpp.so shared object")?;
    let il2cpp_metadata = brocolib::Metadata::parse(&global_metadata_data, &elf_data)?;

    let mut metadata = Metadata {
        metadata: &il2cpp_metadata,
        code_registration: &il2cpp_metadata.runtime_metadata.code_registration,
        metadata_registration: &il2cpp_metadata.runtime_metadata.metadata_registration,
        method_calculations: Default::default(),
        parent_to_child_map: Default::default(),
        child_to_parent_map: Default::default(),
        // TODO: These should come from args to the program?
        custom_type_handler: Default::default(),
        custom_type_resolve_handler: Default::default(),
        name_to_tdi: Default::default(),
        blacklisted_types: Default::default(),
        pointer_size: generate::metadata::PointerSize::Bytes8,
        // For most il2cpp versions
        packing_field_offset: 7,
        size_is_default_offset: 12,
        specified_packing_field_offset: 13,
        packing_is_default_offset: 11,
    };
    let t = time::Instant::now();
    info!("Parsing metadata methods");
    metadata.parse();
    info!("Finished in {}ms", t.elapsed().as_millis());

    if let Some(json) = cli.json {
        println!("Writing json file {json:?}");
        make_json(&metadata, &STATIC_CONFIG, json)?;
        return Ok(());
    }
    if let Some(json_folder) = cli.multi_json {
        println!("Writing json file {json_folder:?}");
        make_json_folder(&metadata, &STATIC_CONFIG, json_folder)?;
        return Ok(());
    }

    let mut cpp_context_collection = CppContextCollection::new();

    // blacklist types
    {
        let mut blacklist_type = |full_name: &str| {
            let tdi = metadata
                .metadata
                .global_metadata
                .type_definitions
                .as_vec()
                .iter()
                .enumerate()
                .find(|(_, t)| t.full_name(metadata.metadata, false) == full_name);

            if let Some((tdi, _td)) = tdi {
                info!("Blacklisted {full_name}");

                metadata
                    .blacklisted_types
                    .insert(TypeDefinitionIndex::new(tdi as u32));
            } else {
                warn!("Unable to blacklist {full_name}")
            }
        };

        blacklist_type("UnityEngine.XR.XRInputSubsystemDescriptor");
        blacklist_type("UnityEngine.XR.XRMeshSubsystemDescriptor");
        blacklist_type("UnityEngine.XR.XRDisplaySubsystem");
        blacklist_type("UIToolkitUtilities.Controls.Table"); // TODO: Make System.Enum work properly
                                                             // blacklist_type("NetworkPacketSerializer`2::<>c__DisplayClass4_0`1");
                                                             // blacklist_type("NetworkPacketSerializer`2::<>c__DisplayClass8_0`1");
                                                             // blacklist_type("NetworkPacketSerializer`2::<>c__DisplayClass7_0`1");
                                                             // blacklist_type("NetworkPacketSerializer`2::<>c__DisplayClass5_0`1");
                                                             // blacklist_type("NetworkPacketSerializer`2::<>c__DisplayClass10_0");
                                                             // blacklist_type("NetworkPacketSerializer`2::<>c__6`1");
                                                             // blacklist_type("RpcHandler`1::<>c__DisplayClass14_0`5");
                                                             // blacklist_type("RpcHandler`1::<>c__DisplayClass10_0`1");
                                                             // blacklist_type("RpcHandler`1::<>c__DisplayClass11_0`2");
                                                             // blacklist_type("RpcHandler`1::<>c__DisplayClass12_0`3");
                                                             // blacklist_type("RpcHandler`1::<>c__DisplayClass13_0`4");
                                                             // blacklist_type("RpcHandler`1::<>c__DisplayClass14_0`5");
                                                             // blacklist_type("RpcHandler`1::<>c__DisplayClass15_0`1");
                                                             // blacklist_type("RpcHandler`1::<>c__DisplayClass16_0`2");
                                                             // blacklist_type("RpcHandler`1::<>c__DisplayClass17_0`3");
                                                             // blacklist_type("RpcHandler`1::<>c__DisplayClass18_0`4");
                                                             // blacklist_type("RpcHandler`1::<>c__DisplayClass19_0`5");

        // Incorrect offsets / sizes due to il2cpp bug
        blacklist_type("UnityEngine.InputSystem.InputInteractionContext");
        blacklist_type("UnityEngine.InputSystem.IInputInteraction");
        blacklist_type("UnityEngine.InputSystem.LowLevel.ActionEvent");
        blacklist_type("UnityEngine.InputSystem.Interactions.HoldInteraction");
        blacklist_type("UnityEngine.InputSystem.Interactions.MultiTapInteraction");
        blacklist_type("UnityEngine.InputSystem.Interactions.PressInteraction");
        blacklist_type("UnityEngine.InputSystem.Interactions.TapInteraction");
        blacklist_type("UnityEngine.InputSystem.Interactions.SlowTapInteraction");
        blacklist_type("UnityEngine.InputSystem.LowLevel.UseWindowsGamingInputCommand");
        blacklist_type("UnityEngine.InputSystem.LowLevel.EnableIMECompositionCommand");
        blacklist_type("UnityEngine.InputSystem.LowLevel.MouseState");
        blacklist_type("UnityEngine.InputSystem.LowLevel.QueryCanRunInBackground");
        blacklist_type("UnityEngine.InputSystem.LowLevel.QueryEnabledStateCommand");
        blacklist_type("UnityEngine.InputSystem.Utilities.InputActionTrace");
        blacklist_type("UnityEngine.InputSystem.Utilities.InputActionTrace::ActionEventPtr");
        blacklist_type("UnityEngine.InputSystem.Utilities.InputActionTrace::Enumerator");
        blacklist_type("System.MonoLimitationAttribute");
    }
    {
        let _blacklist_types = |full_name: &str| {
            let tdis = metadata
                .metadata
                .global_metadata
                .type_definitions
                .as_vec()
                .iter()
                .enumerate()
                .filter(|(_, t)| t.full_name(metadata.metadata, false).contains(full_name))
                .collect_vec();

            match tdis.is_empty() {
                true => warn!("Unable to blacklist {full_name}"),
                false => {
                    for (tdi, td) in tdis {
                        info!("Blacklisted {}", td.full_name(metadata.metadata, true));

                        metadata
                            .blacklisted_types
                            .insert(TypeDefinitionIndex::new(tdi as u32));
                    }
                }
            }
        };
        // blacklist_types("<>c__DisplayClass");
    }
    {
        // First, make all the contexts
        info!("Making types");
        let type_defs = metadata.metadata.global_metadata.type_definitions.as_vec();
        let total = type_defs.len();
        for tdi_u64 in 0..total {
            let tdi = TypeDefinitionIndex::new(tdi_u64 as u32);

            let ty_def = &metadata.metadata.global_metadata.type_definitions[tdi];
            let _ty = &metadata.metadata_registration.types[ty_def.byval_type_index as usize];

            if ty_def.declaring_type_index != u32::MAX {
                continue;
            }

            trace!(
                "Making types {:.4}% ({tdi_u64}/{total})",
                (tdi_u64 as f64 / total as f64 * 100.0)
            );
            cpp_context_collection.make_from(
                &metadata,
                &STATIC_CONFIG,
                TypeData::TypeDefinitionIndex(tdi),
                None,
            );
            cpp_context_collection.alias_nested_types_il2cpp(
                tdi,
                CppTypeTag::TypeDefinitionIndex(tdi),
                &metadata,
                false,
            );
        }
    }
    {
        // First, make all the contexts
        info!("Making nested types");
        let type_defs = metadata.metadata.global_metadata.type_definitions.as_vec();
        let total = type_defs.len();
        for tdi_u64 in 0..total {
            let tdi = TypeDefinitionIndex::new(tdi_u64 as u32);

            let ty_def = &metadata.metadata.global_metadata.type_definitions[tdi];

            if ty_def.declaring_type_index == u32::MAX {
                continue;
            }

            trace!(
                "Making nested types {:.4}% ({tdi_u64}/{total})",
                (tdi_u64 as f64 / total as f64 * 100.0)
            );
            cpp_context_collection.make_nested_from(&metadata, &STATIC_CONFIG, tdi, None);
        }
    }

    // {
    //     let total = metadata.metadata_registration.generic_method_table.len() as f64;
    //     info!("Making generic type instantiations");
    //     for (i, generic_class) in metadata
    //         .metadata_registration
    //         .generic_method_table
    //         .iter()
    //         .enumerate()
    //     {
    //         trace!(
    //             "Making generic type instantiations {:.4}% ({i}/{total})",
    //             (i as f64 / total * 100.0)
    //         );
    //         let method_spec = metadata
    //             .metadata_registration
    //             .method_specs
    //             .get(generic_class.generic_method_index as usize)
    //             .unwrap();

    //         cpp_context_collection.make_generic_from(method_spec, &mut metadata, &STATIC_CONFIG);
    //     }
    // }
    // {
    //     let total = metadata.metadata_registration.generic_method_table.len() as f64;
    //     info!("Filling generic types!");
    //     for (i, generic_class) in metadata
    //         .metadata_registration
    //         .generic_method_table
    //         .iter()
    //         .enumerate()
    //     {
    //         trace!(
    //             "Filling generic type instantiations {:.4}% ({i}/{total})",
    //             (i as f64 / total * 100.0)
    //         );
    //         let method_spec = metadata
    //             .metadata_registration
    //             .method_specs
    //             .get(generic_class.generic_method_index as usize)
    //             .unwrap();

    //         cpp_context_collection.fill_generic_class_inst(
    //             method_spec,
    //             &mut metadata,
    //             &STATIC_CONFIG,
    //         );
    //     }
    // }

    if cli.gen_generic_methods_specializations {
        let total = metadata.metadata_registration.generic_method_table.len() as f64;
        info!("Filling generic methods!");
        for (i, generic_class) in metadata
            .metadata_registration
            .generic_method_table
            .iter()
            .enumerate()
        {
            trace!(
                "Filling generic method instantiations {:.4}% ({i}/{total})",
                (i as f64 / total * 100.0)
            );
            let method_spec = metadata
                .metadata_registration
                .method_specs
                .get(generic_class.generic_method_index as usize)
                .unwrap();

            cpp_context_collection.fill_generic_method_inst(
                method_spec,
                &mut metadata,
                &STATIC_CONFIG,
            );
        }
    }

    info!("Registering handlers!");
    // il2cpp_internals::register_il2cpp_types(&mut metadata)?;
    unity::register_unity(&mut metadata)?;
    object::register_system(&mut metadata)?;
    value_type::register_value_type(&mut metadata)?;
    info!("Handlers registered!");

    {
        // Fill them now
        info!("Filling types");
        let type_defs = metadata.metadata.global_metadata.type_definitions.as_vec();
        let total = type_defs.len();
        for tdi_u64 in 0..total {
            let tdi = TypeDefinitionIndex::new(tdi_u64 as u32);

            trace!(
                "Filling type {:.4} ({tdi_u64}/{total})",
                (tdi_u64 as f64 / total as f64 * 100.0)
            );

            cpp_context_collection.fill(
                &metadata,
                &STATIC_CONFIG,
                CppTypeTag::TypeDefinitionIndex(tdi),
            );
        }
    }

    if cli.remove_verbose_comments {
        remove_coments(&mut cpp_context_collection)?;
    }

    const write_all: bool = true;
    if write_all {
        cpp_context_collection.write_all(&STATIC_CONFIG)?;
        cpp_context_collection.write_namespace_headers()?;
    } else {
        // for t in &metadata.type_definitions {
        //     // Handle the generation for a single type
        //     let dest = open_writer(&metadata, &config, &t);
        //     write_type(&metadata, &config, &t, &dest);
        // }
        fn make_td_tdi(idx: u32) -> TypeData {
            TypeData::TypeDefinitionIndex(TypeDefinitionIndex::new(idx))
        }
        // All indices require updating
        // cpp_context_collection.get()[&make_td_tdi(123)].write()?;
        // cpp_context_collection.get()[&make_td_tdi(342)].write()?;
        // cpp_context_collection.get()[&make_td_tdi(512)].write()?;
        // cpp_context_collection.get()[&make_td_tdi(1024)].write()?;
        // cpp_context_collection.get()[&make_td_tdi(600)].write()?;
        // cpp_context_collection.get()[&make_td_tdi(1000)].write()?;
        // cpp_context_collection.get()[&make_td_tdi(420)].write()?;
        // cpp_context_collection.get()[&make_td_tdi(69)].write()?;
        // cpp_context_collection.get()[&make_td_tdi(531)].write()?;
        // cpp_context_collection.get()[&make_td_tdi(532)].write()?;
        // cpp_context_collection.get()[&make_td_tdi(533)].write()?;
        // cpp_context_collection.get()[&make_td_tdi(534)].write()?;
        // cpp_context_collection.get()[&make_td_tdi(535)].write()?;
        // cpp_context_collection.get()[&make_td_tdi(1455)].write()?;
        info!("Generic type");
        cpp_context_collection
            .get()
            .iter()
            .find(|(_, c)| c.get_types().iter().any(|(_, t)| t.cpp_template.is_some()))
            .unwrap()
            .1
            .write(&STATIC_CONFIG)?;
        info!("List Generic type");
        cpp_context_collection
            .get()
            .iter()
            .find(|(_, c)| {
                c.get_types().iter().any(|(_, t)| {
                    t.cpp_name_components.generics.is_some() && t.cpp_name() == "List_1"
                })
            })
            .unwrap()
            .1
            .write(&STATIC_CONFIG)?;
        info!("Value type");
        cpp_context_collection
            .get()
            .iter()
            .find(|(_, c)| {
                c.get_types().iter().any(|(_, t)| {
                    t.is_value_type && t.name() == "Color" && t.namespace() == "UnityEngine"
                })
            })
            .unwrap()
            .1
            .write(&STATIC_CONFIG)?;
        // info!("Nested type");
        // cpp_context_collection
        //     .get()
        //     .iter()
        //     .find(|(_, c)| {
        //         c.get_types().iter().any(|(_, t)| {
        //             t.nested_types
        //                 .iter()
        //                 .any(|(_, n)| !n.declarations.is_empty())
        //         })
        //     })
        //     .unwrap()
        //     .1
        //     .write()?;
        // Doesn't exist anymore?
        // info!("AlignmentUnion type");
        // cpp_context_collection
        //     .get()
        //     .iter()
        //     .find(|(_, c)| {
        //         c.get_types()
        //             .iter()
        //             .any(|(_, t)| t.is_value_type && &t.name()== "AlignmentUnion")
        //     })
        //     .unwrap()
        //     .1
        //     .write()?;
        info!("Array type");
        cpp_context_collection
            .get()
            .iter()
            .find(|(_, c)| {
                c.get_types()
                    .iter()
                    .any(|(_, t)| t.name() == "Array" && t.namespace() == "System")
            })
            .unwrap()
            .1
            .write(&STATIC_CONFIG)?;
        info!("Default param");
        cpp_context_collection
            .get()
            .iter()
            .filter(|(_, c)| {
                c.get_types().iter().any(|(_, t)| {
                    t.implementations.iter().any(|d| {
                        if let CppMember::MethodImpl(m) = d.as_ref() {
                            m.parameters.iter().any(|p| p.def_value.is_some())
                        } else {
                            false
                        }
                    })
                })
            })
            .nth(2)
            .unwrap()
            .1
            .write(&STATIC_CONFIG)?;
        info!("Enum type");
        cpp_context_collection
            .get()
            .iter()
            .find(|(_, c)| c.get_types().iter().any(|(_, t)| t.is_enum_type))
            .unwrap()
            .1
            .write(&STATIC_CONFIG)?;
        info!("UnityEngine.Object");
        cpp_context_collection
            .get()
            .iter()
            .find(|(_, c)| {
                c.get_types()
                    .iter()
                    .any(|(_, t)| t.name() == "Object" && t.namespace() == "UnityEngine")
            })
            .unwrap()
            .1
            .write(&STATIC_CONFIG)?;
        info!("BeatmapSaveDataHelpers");
        cpp_context_collection
            .get()
            .iter()
            .find(|(_, c)| {
                c.get_types()
                    .iter()
                    .any(|(_, t)| t.name() == "BeatmapSaveDataHelpers")
            })
            .unwrap()
            .1
            .write(&STATIC_CONFIG)?;
        info!("HMUI.ViewController");
        cpp_context_collection
            .get()
            .iter()
            .find(|(_, c)| {
                c.get_types()
                    .iter()
                    .any(|(_, t)| t.namespace() == "HMUI" && t.name() == "ViewController")
            })
            .unwrap()
            .1
            .write(&STATIC_CONFIG)?;
        info!("UnityEngine.Component");
        cpp_context_collection
            .get()
            .iter()
            .find(|(_, c)| {
                c.get_types()
                    .iter()
                    .any(|(_, t)| t.namespace() == "UnityEngine" && t.name() == "Component")
            })
            .unwrap()
            .1
            .write(&STATIC_CONFIG)?;
        info!("UnityEngine.GameObject");
        cpp_context_collection
            .get()
            .iter()
            .find(|(_, c)| {
                c.get_types()
                    .iter()
                    .any(|(_, t)| t.namespace() == "UnityEngine" && t.name() == "GameObject")
            })
            .unwrap()
            .1
            .write(&STATIC_CONFIG)?;
        info!("MainFlowCoordinator");
        cpp_context_collection
            .get()
            .iter()
            .find(|(_, c)| {
                c.get_types()
                    .iter()
                    .any(|(_, t)| t.namespace().is_empty() && t.name() == "MainFlowCoordinator")
            })
            .unwrap()
            .1
            .write(&STATIC_CONFIG)?;
        info!("OVRPlugin");
        cpp_context_collection
            .get()
            .iter()
            .find(|(_, c)| {
                c.get_types()
                    .iter()
                    .any(|(_, t)| t.namespace().is_empty() && t.name() == "OVRPlugin")
            })
            .unwrap()
            .1
            .write(&STATIC_CONFIG)?;
        info!("HMUI.IValueChanger");
        cpp_context_collection
            .get()
            .iter()
            .find(|(_, c)| {
                c.get_types()
                    .iter()
                    .any(|(_, t)| t.namespace() == "HMUI" && t.name() == "IValueChanger`1")
            })
            .unwrap()
            .1
            .write(&STATIC_CONFIG)?;
        info!("System.ValueType");
        cpp_context_collection
            .get()
            .iter()
            .find(|(_, c)| {
                c.get_types()
                    .iter()
                    .any(|(_, t)| t.namespace() == "System" && t.name() == "ValueType")
            })
            .unwrap()
            .1
            .write(&STATIC_CONFIG)?;
        info!("System.ValueTuple_2");
        cpp_context_collection
            .get()
            .iter()
            .find(|(_, c)| {
                c.get_types()
                    .iter()
                    .any(|(_, t)| t.namespace() == "System" && t.name() == "ValueTuple`2")
            })
            .unwrap()
            .1
            .write(&STATIC_CONFIG)?;
        info!("System.Decimal");
        cpp_context_collection
            .get()
            .iter()
            .find(|(_, c)| {
                c.get_types()
                    .iter()
                    .any(|(_, t)| t.namespace() == "System" && t.name() == "Decimal")
            })
            .unwrap()
            .1
            .write(&STATIC_CONFIG)?;
        info!("System.Enum");
        cpp_context_collection
            .get()
            .iter()
            .find(|(_, c)| {
                c.get_types()
                    .iter()
                    .any(|(_, t)| t.namespace() == "System" && t.name() == "Enum")
            })
            .unwrap()
            .1
            .write(&STATIC_CONFIG)?;
        info!("System.Multicast");
        cpp_context_collection
            .get()
            .iter()
            .find(|(_, c)| {
                c.get_types()
                    .iter()
                    .any(|(_, t)| t.namespace() == "System" && t.name() == "MulticastDelegate")
            })
            .unwrap()
            .1
            .write(&STATIC_CONFIG)?;
        info!("System.Delegate");
        cpp_context_collection
            .get()
            .iter()
            .find(|(_, c)| {
                c.get_types()
                    .iter()
                    .any(|(_, t)| t.namespace() == "System" && t.name() == "Delegate")
            })
            .unwrap()
            .1
            .write(&STATIC_CONFIG)?;
        info!("BeatmapSaveDataVersion3.BeatmapSaveData.EventBoxGroup`1");
        cpp_context_collection
            .get()
            .iter()
            .find(|(_, c)| {
                c.get_types()
                    .iter()
                    .any(|(_, t)| t.name().contains("EventBoxGroup`1"))
            })
            .unwrap()
            .1
            .write(&STATIC_CONFIG)?;
        // for (_, context) in cpp_context_collection.get() {
        //     context.write().unwrap();
        // }
    }

    if cli.format {
        format_files()?;
    }

    Ok(())
}

fn format_files() -> Result<()> {
    info!("Formatting!");

    use walkdir::WalkDir;

    let files: Vec<DirEntry> = WalkDir::new(&STATIC_CONFIG.header_path)
        .into_iter()
        .filter(|f| f.as_ref().is_ok_and(|f| f.path().is_file()))
        .try_collect()?;

    let file_count = files.len();

    info!(
        "{file_count} files across {} threads",
        rayon::current_num_threads()
    );
    // easily get file size for a given file
    fn file_size(file: &DirEntry) -> usize {
        match std::fs::metadata(file.path()) {
            Ok(data) => file.path().size_on_disk_fast(&data).unwrap() as usize,
            Err(_) => 0,
        }
    }

    // TODO: Debug
    warn!("Do not run with debugger, for some reason an early abrupt exit.");

    files
        .iter()
        // sort on file size
        .sorted_by(|a, b| file_size(a).cmp(&file_size(b)))
        // reverse to go big -> small, so we can work on other files while big files are happening
        .rev()
        // parallelism
        .enumerate()
        .par_bridge()
        .try_for_each(|(file_num, file)| -> Result<()> {
            let path = file.path();
            info!(
                "Formatting [{}/{file_count}] {}",
                file_num + 1,
                path.display()
            );
            let mut command = Command::new("clang-format");
            command.arg("-i").arg(path);

            let spawn = command
                .output()
                .suggestion("You may be missing clang-format. Ensure it is on PATH")?;

            if !spawn.stderr.is_empty() {
                error!(
                    "Error {} {}",
                    path.display(),
                    String::from_utf8(spawn.stderr)?
                );
            }

            spawn.status.exit_ok()?;

            Ok(())
        })?;

    info!("Done formatting!");
    Ok(())
}
