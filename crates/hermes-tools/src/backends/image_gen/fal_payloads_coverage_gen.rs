mod backends {
    mod image_gen {
        mod fal_payloads {
            mod coverage_gen {
                use super::super::super::super::super::*;

                #[test]
                fn resolve_krea_model_prefers_explicit_valid_model() {
                    let first = KREA_MODELS
                        .first()
                        .copied()
                        .expect("at least one Krea model");
                    let explicit = format!("  {}  ", first.id.to_ascii_uppercase());
                    let spec = resolve_krea_model(Some(&explicit));
                    assert_eq!(spec.id, first.id);
                }

                #[test]
                fn resolve_krea_creativity_prefers_explicit_valid_value() {
                    assert_eq!(resolve_krea_creativity(Some("  HIGH ")), "high");
                    assert_eq!(resolve_krea_creativity(Some("raw")), "raw");
                }

                #[test]
                fn normalize_krea_creativity_rejects_unknown() {
                    assert_eq!(normalize_krea_creativity("bogus"), None);
                    assert_eq!(normalize_krea_creativity(""), None);
                    assert_eq!(normalize_krea_creativity(" Low "), Some("low"));
                }

                #[test]
                fn normalize_fal_model_path_handles_empty_and_unknown() {
                    assert_eq!(normalize_fal_model_path("   "), None);
                    assert_eq!(normalize_fal_model_path(""), None);
                    assert_eq!(normalize_fal_model_path("nonsense"), None);
                    assert_eq!(
                        normalize_fal_model_path(" custom/model "),
                        Some("custom/model".to_string())
                    );
                    assert_eq!(
                        normalize_fal_model_path("fal-ai/qwen-image"),
                        Some("fal-ai/qwen-image".to_string())
                    );
                }

                #[test]
                fn fal_aspect_from_tool_size_maps_all_variants() {
                    assert_eq!(fal_aspect_from_tool_size(Some("square")), "square");
                    assert_eq!(fal_aspect_from_tool_size(Some(" 1:1 ")), "square");
                    assert_eq!(fal_aspect_from_tool_size(Some("512x512")), "square");
                    assert_eq!(fal_aspect_from_tool_size(Some("256x256")), "square");
                    assert_eq!(fal_aspect_from_tool_size(Some("1024X1024")), "square");
                    assert_eq!(fal_aspect_from_tool_size(Some("portrait")), "portrait");
                    assert_eq!(fal_aspect_from_tool_size(Some("16:9")), "landscape");
                    assert_eq!(
                        fal_aspect_from_tool_size(Some("weird-size")),
                        DEFAULT_FAL_ASPECT_RATIO
                    );
                    assert_eq!(fal_aspect_from_tool_size(None), DEFAULT_FAL_ASPECT_RATIO);
                }

                #[test]
                fn fal_model_defaults_klein() {
                    let p = fal_model_defaults("fal-ai/flux-2/klein/9b");
                    assert_eq!(p.get("num_inference_steps"), Some(&serde_json::json!(4)));
                    assert_eq!(p.get("output_format"), Some(&serde_json::json!("png")));
                    assert_eq!(
                        p.get("enable_safety_checker"),
                        Some(&serde_json::json!(false))
                    );
                    assert_eq!(p.len(), 3);
                }

                #[test]
                fn fal_model_defaults_flux_pro() {
                    let p = fal_model_defaults("fal-ai/flux-2-pro");
                    assert_eq!(p.get("num_inference_steps"), Some(&serde_json::json!(50)));
                    assert_eq!(p.get("guidance_scale"), Some(&serde_json::json!(4.5)));
                    assert_eq!(p.get("num_images"), Some(&serde_json::json!(1)));
                    assert_eq!(p.get("output_format"), Some(&serde_json::json!("png")));
                    assert_eq!(
                        p.get("enable_safety_checker"),
                        Some(&serde_json::json!(false))
                    );
                    assert_eq!(p.get("safety_tolerance"), Some(&serde_json::json!("5")));
                    assert_eq!(p.get("sync_mode"), Some(&serde_json::json!(true)));
                    assert_eq!(p.len(), 7);
                }

                #[test]
                fn fal_model_defaults_z_image() {
                    let p = fal_model_defaults("fal-ai/z-image/turbo");
                    assert_eq!(p.get("num_inference_steps"), Some(&serde_json::json!(8)));
                    assert_eq!(p.get("num_images"), Some(&serde_json::json!(1)));
                    assert_eq!(p.get("output_format"), Some(&serde_json::json!("png")));
                    assert_eq!(
                        p.get("enable_safety_checker"),
                        Some(&serde_json::json!(false))
                    );
                    assert_eq!(
                        p.get("enable_prompt_expansion"),
                        Some(&serde_json::json!(false))
                    );
                    assert_eq!(p.len(), 5);
                }

                #[test]
                fn fal_model_defaults_ideogram_recraft_qwen_krea_and_unknown() {
                    let p = fal_model_defaults("fal-ai/ideogram/v3");
                    assert_eq!(
                        p.get("rendering_speed"),
                        Some(&serde_json::json!("BALANCED"))
                    );
                    assert_eq!(p.get("expand_prompt"), Some(&serde_json::json!(true)));
                    assert_eq!(p.get("style"), Some(&serde_json::json!("AUTO")));
                    assert_eq!(p.len(), 3);

                    let p = fal_model_defaults("fal-ai/recraft/v4/pro/text-to-image");
                    assert_eq!(
                        p.get("enable_safety_checker"),
                        Some(&serde_json::json!(false))
                    );
                    assert_eq!(p.len(), 1);

                    let p = fal_model_defaults("fal-ai/qwen-image");
                    assert_eq!(p.get("num_inference_steps"), Some(&serde_json::json!(30)));
                    assert_eq!(p.get("guidance_scale"), Some(&serde_json::json!(2.5)));
                    assert_eq!(p.get("num_images"), Some(&serde_json::json!(1)));
                    assert_eq!(p.get("output_format"), Some(&serde_json::json!("png")));
                    assert_eq!(p.get("acceleration"), Some(&serde_json::json!("regular")));
                    assert_eq!(p.len(), 5);

                    for model in [
                        "fal-ai/krea/v2/medium/text-to-image",
                        "fal-ai/krea/v2/large/text-to-image",
                    ] {
                        let p = fal_model_defaults(model);
                        assert_eq!(p.get("creativity"), Some(&serde_json::json!("medium")));
                        assert_eq!(p.len(), 1);
                    }

                    assert!(fal_model_defaults("unknown/model").is_empty());
                }

                #[test]
                fn fal_model_spec_gpt_image_15() {
                    let s = fal_model_spec(" fal-ai/gpt-image-1.5 ").expect("spec");
                    assert_eq!(s.id, "fal-ai/gpt-image-1.5");
                    assert_eq!(s.display, "GPT Image 1.5");
                    assert!(matches!(s.size_style, FalSizeStyle::GptLiteral));
                    assert_eq!(s.landscape, "1536x1024");
                    assert_eq!(s.square, "1024x1024");
                    assert_eq!(s.portrait, "1024x1536");
                    assert!(s.supports.contains(&"background"));
                    assert!(s.edit_supports.contains(&"image_urls"));
                    assert!(!s.edit_supports.contains(&"background"));
                    assert_eq!(s.edit_endpoint, Some("fal-ai/gpt-image-1.5/edit"));
                    assert_eq!(s.max_reference_images, 16);
                }

                #[test]
                fn fal_model_spec_ideogram() {
                    let s = fal_model_spec("fal-ai/ideogram/v3").expect("spec");
                    assert_eq!(s.id, "fal-ai/ideogram/v3");
                    assert_eq!(s.display, "Ideogram V3");
                    assert!(matches!(s.size_style, FalSizeStyle::ImageSizePreset));
                    assert_eq!(s.landscape, "landscape_16_9");
                    assert_eq!(s.square, "square_hd");
                    assert_eq!(s.portrait, "portrait_16_9");
                    assert!(s.supports.contains(&"style"));
                    assert!(s.edit_supports.contains(&"rendering_speed"));
                    assert_eq!(s.edit_endpoint, Some("fal-ai/ideogram/v3/edit"));
                    assert_eq!(s.max_reference_images, 1);
                }

                #[test]
                fn fal_model_spec_recraft() {
                    let s = fal_model_spec("fal-ai/recraft/v4/pro/text-to-image").expect("spec");
                    assert_eq!(s.id, "fal-ai/recraft/v4/pro/text-to-image");
                    assert_eq!(s.display, "Recraft V4 Pro");
                    assert!(matches!(s.size_style, FalSizeStyle::ImageSizePreset));
                    assert!(s.supports.contains(&"colors"));
                    assert!(s.supports.contains(&"background_color"));
                    assert!(s.edit_supports.is_empty());
                    assert_eq!(s.edit_endpoint, None);
                    assert_eq!(s.max_reference_images, 0);
                }

                #[test]
                fn fal_model_spec_qwen() {
                    let s = fal_model_spec("fal-ai/qwen-image").expect("spec");
                    assert_eq!(s.id, "fal-ai/qwen-image");
                    assert_eq!(s.display, "Qwen Image");
                    assert!(matches!(s.size_style, FalSizeStyle::ImageSizePreset));
                    assert!(s.supports.contains(&"acceleration"));
                    assert!(s.edit_supports.contains(&"image_urls"));
                    assert_eq!(s.edit_endpoint, Some("fal-ai/qwen-image-2/pro/edit"));
                    assert_eq!(s.max_reference_images, 3);
                }

                #[test]
                fn fal_model_spec_krea_variants() {
                    let m = fal_model_spec("fal-ai/krea/v2/medium/text-to-image").expect("spec");
                    assert_eq!(m.id, "fal-ai/krea/v2/medium/text-to-image");
                    assert_eq!(m.display, "Krea 2 Medium");
                    assert!(matches!(m.size_style, FalSizeStyle::AspectRatio));
                    assert_eq!(m.landscape, "16:9");
                    assert_eq!(m.square, "1:1");
                    assert_eq!(m.portrait, "9:16");
                    assert!(m.supports.contains(&"creativity"));
                    assert!(m.supports.contains(&"image_style_references"));
                    assert!(m.edit_supports.is_empty());
                    assert_eq!(m.edit_endpoint, None);
                    assert_eq!(m.max_reference_images, 0);

                    let l = fal_model_spec("fal-ai/krea/v2/large/text-to-image").expect("spec");
                    assert_eq!(l.id, "fal-ai/krea/v2/large/text-to-image");
                    assert_eq!(l.display, "Krea 2 Large");
                    assert!(matches!(l.size_style, FalSizeStyle::AspectRatio));
                    assert_eq!(l.landscape, "16:9");
                    assert!(l.supports.contains(&"aspect_ratio"));
                    assert!(l.edit_supports.is_empty());
                    assert_eq!(l.edit_endpoint, None);
                    assert_eq!(l.max_reference_images, 0);
                }

                #[test]
                fn fal_model_spec_unknown_is_none() {
                    assert!(fal_model_spec("fal-ai/does-not-exist").is_none());
                }

                #[test]
                fn insert_fal_size_for_aspect_ratio_and_gpt_literal_models() {
                    let krea = fal_model_spec("fal-ai/krea/v2/large/text-to-image").expect("spec");
                    let mut payload = serde_json::Map::new();
                    insert_fal_size(&mut payload, krea, krea.supports, Some("portrait"));
                    assert_eq!(
                        payload.get("aspect_ratio"),
                        Some(&serde_json::json!("9:16"))
                    );
                    assert!(!payload.contains_key("image_size"));

                    let gpt = fal_model_spec("fal-ai/gpt-image-1.5").expect("spec");
                    let mut payload = serde_json::Map::new();
                    insert_fal_size(&mut payload, gpt, gpt.supports, Some("square"));
                    assert_eq!(
                        payload.get("image_size"),
                        Some(&serde_json::json!("1024x1024"))
                    );

                    let mut payload = serde_json::Map::new();
                    insert_fal_size(&mut payload, gpt, gpt.edit_supports, Some("16:9"));
                    assert_eq!(
                        payload.get("image_size"),
                        Some(&serde_json::json!("1536x1024"))
                    );
                }
            }
        }
    }
}
