//! Form XObject and image XObject extraction.

use super::fonts::descriptor_style_flags;
use crate::text_utils::{effective_font_size, expand_ligatures, is_bold_font, is_italic_font};
use crate::tounicode::FontCMaps;
use crate::types::{ItemType, TextItem};
use lopdf::{Document, Encoding, Object, ObjectId};
use std::collections::HashMap;

use super::fonts::{
    build_font_encodings, build_font_widths, compute_string_width_ts, extract_text_from_operand,
    get_font_file2_obj_num, get_operand_bytes, CMapDecisionCache, FontStyleCache,
};
use super::{get_number, image_bbox_from_ctm, multiply_matrices};

const MAX_FORM_XOBJECT_DEPTH: u8 = 5;

pub(crate) enum XObjectType {
    Image,
    Form(ObjectId),
}

/// Get XObjects from page resources, categorized by type
pub(crate) fn get_page_xobjects(
    doc: &Document,
    page_id: ObjectId,
) -> std::collections::HashMap<String, XObjectType> {
    let mut xobject_types = std::collections::HashMap::new();

    // Try to get the page dictionary
    if let Ok(page_dict) = doc.get_dictionary(page_id) {
        // Get Resources dictionary
        let resources = if let Ok(res_ref) = page_dict.get(b"Resources") {
            if let Ok(obj_ref) = res_ref.as_reference() {
                doc.get_dictionary(obj_ref).ok()
            } else {
                res_ref.as_dict().ok()
            }
        } else {
            None
        };

        if let Some(resources) = resources {
            collect_xobjects_from_dict(doc, resources, &mut xobject_types);
        }
    }

    xobject_types
}

/// Get XObjects from a Form XObject's Resources
fn get_form_xobjects(
    doc: &Document,
    form_dict: &lopdf::Dictionary,
) -> HashMap<String, XObjectType> {
    let mut xobject_types = HashMap::new();

    let resources = if let Ok(res_ref) = form_dict.get(b"Resources") {
        if let Ok(obj_ref) = res_ref.as_reference() {
            doc.get_dictionary(obj_ref).ok()
        } else {
            res_ref.as_dict().ok()
        }
    } else {
        return xobject_types;
    };

    if let Some(resources) = resources {
        collect_xobjects_from_dict(doc, resources, &mut xobject_types);
    }

    xobject_types
}

/// Collect XObject entries from a Resources dictionary
fn collect_xobjects_from_dict(
    doc: &Document,
    resources: &lopdf::Dictionary,
    xobject_types: &mut HashMap<String, XObjectType>,
) {
    if let Ok(xobjects_ref) = resources.get(b"XObject") {
        let xobjects = if let Ok(obj_ref) = xobjects_ref.as_reference() {
            doc.get_dictionary(obj_ref).ok()
        } else {
            xobjects_ref.as_dict().ok()
        };

        if let Some(xobjects) = xobjects {
            for (name, value) in xobjects.iter() {
                let name_str = String::from_utf8_lossy(name).to_string();

                if let Ok(obj_ref) = value.as_reference() {
                    if let Ok(Object::Stream(stream)) = doc.get_object(obj_ref) {
                        if let Ok(subtype) = stream.dict.get(b"Subtype") {
                            if let Ok(subtype_name) = subtype.as_name() {
                                if subtype_name == b"Image" {
                                    xobject_types.insert(name_str, XObjectType::Image);
                                } else if subtype_name == b"Form" {
                                    xobject_types.insert(name_str, XObjectType::Form(obj_ref));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Extract text items from a Form XObject
pub(crate) fn extract_form_xobject_text(
    doc: &Document,
    form_id: ObjectId,
    page_num: u32,
    font_cmaps: &FontCMaps,
    parent_ctm: &[f32; 6],
    cmap_decisions: &mut CMapDecisionCache,
    style_cache: &mut FontStyleCache,
) -> Vec<TextItem> {
    extract_form_xobject_text_inner(
        doc,
        form_id,
        page_num,
        font_cmaps,
        parent_ctm,
        cmap_decisions,
        style_cache,
        0,
    )
}

#[allow(clippy::too_many_arguments)]
fn extract_form_xobject_text_inner(
    doc: &Document,
    form_id: ObjectId,
    page_num: u32,
    font_cmaps: &FontCMaps,
    parent_ctm: &[f32; 6],
    cmap_decisions: &mut CMapDecisionCache,
    style_cache: &mut FontStyleCache,
    depth: u8,
) -> Vec<TextItem> {
    use lopdf::content::Content;

    let mut items = Vec::new();

    // Get the Form XObject stream
    let Ok(Object::Stream(stream)) = doc.get_object(form_id) else {
        return items;
    };

    // Decompress the content stream (fall back to raw bytes for uncompressed streams)
    let content_data = match stream.decompressed_content() {
        Ok(data) => data,
        Err(_) => stream.content.clone(),
    };

    // Decode the content stream
    let Ok(content) = Content::decode(&content_data) else {
        return items;
    };

    // Get fonts from the Form's Resources
    let form_fonts = get_form_fonts(doc, &stream.dict);
    let (font_encodings, _has_gid_fonts) = build_font_encodings(doc, &form_fonts, font_cmaps);

    // Build font width info for the form
    let font_widths = build_font_widths(doc, &form_fonts);

    // Build font base names and ToUnicode refs for the form
    let mut font_base_names: HashMap<String, String> = HashMap::new();
    let mut font_tounicode_refs: HashMap<String, u32> = HashMap::new();
    let mut inline_cmaps: HashMap<String, crate::tounicode::CMapEntry> = HashMap::new();

    let mut font_style_flags: HashMap<String, (bool, bool)> = HashMap::new();
    for (font_name, font_dict) in &form_fonts {
        let resource_name = String::from_utf8_lossy(font_name).to_string();
        if let Ok(base_font) = font_dict.get(b"BaseFont") {
            if let Ok(name) = base_font.as_name() {
                let base_name = String::from_utf8_lossy(name).to_string();
                font_base_names.insert(resource_name.clone(), base_name);
            }
        }
        let style = descriptor_style_flags(doc, font_dict, style_cache);
        if style != (false, false) {
            font_style_flags.insert(resource_name.clone(), style);
        }
        match font_dict.get(b"ToUnicode") {
            Ok(tounicode) => {
                if let Ok(obj_ref) = tounicode.as_reference() {
                    font_tounicode_refs.insert(resource_name, obj_ref.0);
                } else if let Object::Stream(s) = tounicode {
                    let data = s
                        .decompressed_content()
                        .unwrap_or_else(|_| s.content.clone());
                    if let Some(entry) =
                        crate::tounicode::build_cmap_entry_from_stream(&data, font_dict, doc, 0)
                    {
                        inline_cmaps.insert(resource_name, entry);
                    }
                }
            }
            Err(_) => {
                if let Some(ff2_obj_num) = get_font_file2_obj_num(doc, font_dict) {
                    font_tounicode_refs.insert(resource_name, ff2_obj_num);
                }
            }
        }
    }

    // Cache font encodings for form fonts
    let mut encoding_cache: HashMap<String, Encoding<'_>> = HashMap::new();
    for (font_name, font_dict) in &form_fonts {
        let name = String::from_utf8_lossy(font_name).to_string();
        if let Ok(enc) = font_dict.get_font_encoding(doc) {
            encoding_cache.insert(name, enc);
        }
    }

    // Build XObject map from the Form's own Resources for nested Do
    let form_xobjects = get_form_xobjects(doc, &stream.dict);

    // Apply the Form XObject's own Matrix (if any) to the parent CTM
    let form_matrix = if let Ok(matrix_obj) = stream.dict.get(b"Matrix") {
        if let Ok(arr) = matrix_obj.as_array() {
            if arr.len() >= 6 {
                let mut m = [1.0f32, 0.0, 0.0, 1.0, 0.0, 0.0];
                for (i, v) in arr.iter().take(6).enumerate() {
                    m[i] = get_number(v).unwrap_or(if i == 0 || i == 3 { 1.0 } else { 0.0 });
                }
                m
            } else {
                [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]
            }
        } else {
            [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]
        }
    } else {
        [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]
    };
    let base_ctm = multiply_matrices(&form_matrix, parent_ctm);

    // Process the content stream
    let mut current_font = String::new();
    let mut current_font_size: f32 = 12.0;
    let mut text_matrix = [1.0f32, 0.0, 0.0, 1.0, 0.0, 0.0];
    let mut in_text_block = false;
    let mut fill_is_white = false;
    let mut ctm = base_ctm;
    let mut ctm_stack: Vec<[f32; 6]> = Vec::new();

    for op in &content.operations {
        match op.operator.as_str() {
            "q" => {
                ctm_stack.push(ctm);
            }
            "Q" => {
                if let Some(saved) = ctm_stack.pop() {
                    ctm = saved;
                }
            }
            "cm" => {
                if op.operands.len() >= 6 {
                    let mut m = [0.0f32; 6];
                    for (i, operand) in op.operands.iter().take(6).enumerate() {
                        m[i] = get_number(operand).unwrap_or(0.0);
                    }
                    ctm = multiply_matrices(&m, &ctm);
                }
            }
            "Do" => {
                if !op.operands.is_empty() {
                    if let Ok(name) = op.operands[0].as_name() {
                        let xobj_name = String::from_utf8_lossy(name).to_string();
                        match form_xobjects.get(&xobj_name) {
                            Some(XObjectType::Form(nested_id)) => {
                                if depth < MAX_FORM_XOBJECT_DEPTH {
                                    let nested_items = extract_form_xobject_text_inner(
                                        doc,
                                        *nested_id,
                                        page_num,
                                        font_cmaps,
                                        &ctm,
                                        cmap_decisions,
                                        style_cache,
                                        depth + 1,
                                    );
                                    items.extend(nested_items);
                                }
                            }
                            Some(XObjectType::Image) => {
                                // Mirror the top-level Image-XObject emission
                                // in content_stream.rs so figures embedded
                                // inside Form XObjects (common in print-to-PDF
                                // workflows) aren't silently dropped.
                                let (x, y, width, height) = image_bbox_from_ctm(&ctm);
                                items.push(TextItem {
                                    text: format!("[Image: {}]", xobj_name),
                                    x,
                                    y,
                                    width,
                                    height,
                                    font: String::new(),
                                    font_size: 0.0,
                                    page: page_num,
                                    is_bold: false,
                                    is_italic: false,
                                    is_underline: false,
                                    is_strikeout: false,
                                    item_type: ItemType::Image,
                                    mcid: None,
                                });
                            }
                            None => {}
                        }
                    }
                }
            }
            "BT" => {
                in_text_block = true;
                text_matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
            }
            "ET" => {
                in_text_block = false;
            }
            "Tf" => {
                if op.operands.len() >= 2 {
                    if let Ok(name) = op.operands[0].as_name() {
                        current_font = String::from_utf8_lossy(name).to_string();
                    }
                    current_font_size = get_number(&op.operands[1]).unwrap_or(12.0);
                }
            }
            "Td" | "TD" => {
                if op.operands.len() >= 2 {
                    let tx = get_number(&op.operands[0]).unwrap_or(0.0);
                    let ty = get_number(&op.operands[1]).unwrap_or(0.0);
                    text_matrix[4] += tx * text_matrix[0] + ty * text_matrix[2];
                    text_matrix[5] += tx * text_matrix[1] + ty * text_matrix[3];
                }
            }
            "Tm" => {
                if op.operands.len() >= 6 {
                    for (i, operand) in op.operands.iter().take(6).enumerate() {
                        text_matrix[i] =
                            get_number(operand).unwrap_or(if i == 0 || i == 3 { 1.0 } else { 0.0 });
                    }
                }
            }
            "g" => {
                if let Some(gray) = op.operands.first().and_then(get_number) {
                    fill_is_white = gray > 0.95;
                }
            }
            "rg" => {
                if op.operands.len() >= 3 {
                    let r = get_number(&op.operands[0]).unwrap_or(0.0);
                    let g = get_number(&op.operands[1]).unwrap_or(0.0);
                    let b = get_number(&op.operands[2]).unwrap_or(0.0);
                    fill_is_white = r > 0.95 && g > 0.95 && b > 0.95;
                }
            }
            "k" => {
                if op.operands.len() >= 4 {
                    let c = get_number(&op.operands[0]).unwrap_or(1.0);
                    let m = get_number(&op.operands[1]).unwrap_or(1.0);
                    let y = get_number(&op.operands[2]).unwrap_or(1.0);
                    let k = get_number(&op.operands[3]).unwrap_or(1.0);
                    fill_is_white = c < 0.05 && m < 0.05 && y < 0.05 && k < 0.05;
                }
            }
            "sc" | "scn" => {
                let nums: Vec<f32> = op.operands.iter().filter_map(get_number).collect();
                match nums.len() {
                    3 => {
                        fill_is_white = nums[0] > 0.95 && nums[1] > 0.95 && nums[2] > 0.95;
                    }
                    4 => {
                        fill_is_white =
                            nums[0] < 0.05 && nums[1] < 0.05 && nums[2] < 0.05 && nums[3] < 0.05;
                    }
                    _ => fill_is_white = false,
                }
            }
            "Tj" => {
                if in_text_block && !op.operands.is_empty() {
                    if fill_is_white {
                        if let Some(font_info) = font_widths.get(&current_font) {
                            if let Some(raw_bytes) = get_operand_bytes(&op.operands[0]) {
                                let w_ts = compute_string_width_ts(
                                    raw_bytes,
                                    font_info,
                                    current_font_size,
                                    0.0,
                                    0.0,
                                );
                                text_matrix[4] += w_ts * text_matrix[0];
                                text_matrix[5] += w_ts * text_matrix[1];
                            }
                        }
                        continue;
                    }
                    if let Some(text) = extract_text_from_operand(
                        &op.operands[0],
                        &current_font,
                        font_base_names.get(&current_font).map(|s| s.as_str()),
                        font_cmaps,
                        &font_tounicode_refs,
                        &inline_cmaps,
                        &font_encodings,
                        &encoding_cache,
                        cmap_decisions,
                        &font_widths,
                    ) {
                        let combined = multiply_matrices(&text_matrix, &ctm);
                        let rendered_size = effective_font_size(current_font_size, &combined);
                        let (x, y) = (combined[4], combined[5]);
                        let width = if let Some(font_info) = font_widths.get(&current_font) {
                            if let Some(raw_bytes) = get_operand_bytes(&op.operands[0]) {
                                let w_ts = compute_string_width_ts(
                                    raw_bytes,
                                    font_info,
                                    current_font_size,
                                    0.0,
                                    0.0,
                                );
                                text_matrix[4] += w_ts * text_matrix[0];
                                text_matrix[5] += w_ts * text_matrix[1];
                                (w_ts * (text_matrix[0] * ctm[0] + text_matrix[1] * ctm[2])).abs()
                            } else {
                                0.0
                            }
                        } else {
                            0.0
                        };
                        // Only create text item for non-whitespace; whitespace
                        // still advances the text matrix above so gap detection works
                        if !text.trim().is_empty() {
                            let base_font = font_base_names
                                .get(&current_font)
                                .map(|s| s.as_str())
                                .unwrap_or(&current_font);
                            let (desc_italic, desc_bold) = font_style_flags
                                .get(&current_font)
                                .copied()
                                .unwrap_or((false, false));
                            items.push(TextItem {
                                text: expand_ligatures(&text),
                                x,
                                y,
                                width,
                                height: rendered_size,
                                font: current_font.clone(),
                                font_size: rendered_size,
                                page: page_num,
                                is_bold: is_bold_font(base_font) || desc_bold,
                                is_italic: is_italic_font(base_font) || desc_italic,
                                is_underline: false,
                                is_strikeout: false,
                                item_type: ItemType::Text,
                                mcid: None,
                            });
                        }
                    }
                }
            }
            "TJ" => {
                // Show text with positioning — split at column-sized gaps
                if in_text_block && !op.operands.is_empty() {
                    if let Ok(array) = op.operands[0].as_array() {
                        let font_info = font_widths.get(&current_font);

                        let space_threshold = if let Some(fi) = font_info {
                            let space_em = fi.space_width as f32 * fi.units_scale;
                            let threshold = space_em * 1000.0 * 0.4;
                            threshold.max(80.0)
                        } else {
                            120.0
                        };
                        let column_gap_threshold = space_threshold * 4.0;

                        let mut sub_items: Vec<(String, f32, f32)> = Vec::new();
                        let mut current_text = String::new();
                        let mut sub_start_width_ts: f32 = 0.0;
                        let mut total_width_ts: f32 = 0.0;
                        for element in array {
                            match element {
                                Object::Integer(n) => {
                                    let n_val = *n as f32;
                                    let displacement = -n_val / 1000.0 * current_font_size;
                                    if !fill_is_white
                                        && n_val < -column_gap_threshold
                                        && !current_text.is_empty()
                                    {
                                        sub_items.push((
                                            std::mem::take(&mut current_text),
                                            sub_start_width_ts,
                                            total_width_ts,
                                        ));
                                        total_width_ts += displacement;
                                        sub_start_width_ts = total_width_ts;
                                    } else {
                                        total_width_ts += displacement;
                                        if !fill_is_white
                                            && n_val < -space_threshold
                                            && !current_text.is_empty()
                                            && !current_text.ends_with(' ')
                                        {
                                            current_text.push(' ');
                                        }
                                    }
                                    continue;
                                }
                                Object::Real(n) => {
                                    let n_val = *n;
                                    let displacement = -n_val / 1000.0 * current_font_size;
                                    if !fill_is_white
                                        && n_val < -column_gap_threshold
                                        && !current_text.is_empty()
                                    {
                                        sub_items.push((
                                            std::mem::take(&mut current_text),
                                            sub_start_width_ts,
                                            total_width_ts,
                                        ));
                                        total_width_ts += displacement;
                                        sub_start_width_ts = total_width_ts;
                                    } else {
                                        total_width_ts += displacement;
                                        if !fill_is_white
                                            && n_val < -space_threshold
                                            && !current_text.is_empty()
                                            && !current_text.ends_with(' ')
                                        {
                                            current_text.push(' ');
                                        }
                                    }
                                    continue;
                                }
                                _ => {}
                            }
                            if let Some(fi) = font_info {
                                if let Some(raw_bytes) = get_operand_bytes(element) {
                                    total_width_ts += compute_string_width_ts(
                                        raw_bytes,
                                        fi,
                                        current_font_size,
                                        0.0,
                                        0.0,
                                    );
                                }
                            }
                            if !fill_is_white {
                                if let Some(text) = extract_text_from_operand(
                                    element,
                                    &current_font,
                                    font_base_names.get(&current_font).map(|s| s.as_str()),
                                    font_cmaps,
                                    &font_tounicode_refs,
                                    &inline_cmaps,
                                    &font_encodings,
                                    &encoding_cache,
                                    cmap_decisions,
                                    &font_widths,
                                ) {
                                    current_text.push_str(&text);
                                }
                            }
                        }
                        if !fill_is_white && !current_text.trim().is_empty() {
                            sub_items.push((current_text, sub_start_width_ts, total_width_ts));
                        }
                        if !sub_items.is_empty() {
                            let combined = multiply_matrices(&text_matrix, &ctm);
                            let rendered_size = effective_font_size(current_font_size, &combined);
                            let base_font = font_base_names
                                .get(&current_font)
                                .map(|s| s.as_str())
                                .unwrap_or(&current_font);
                            let (desc_italic, desc_bold) = font_style_flags
                                .get(&current_font)
                                .copied()
                                .unwrap_or((false, false));
                            let scale_x = text_matrix[0] * ctm[0] + text_matrix[1] * ctm[2];
                            for (text, start_w, end_w) in &sub_items {
                                let offset_tm = [
                                    text_matrix[0],
                                    text_matrix[1],
                                    text_matrix[2],
                                    text_matrix[3],
                                    text_matrix[4] + start_w * text_matrix[0],
                                    text_matrix[5] + start_w * text_matrix[1],
                                ];
                                let combined_mat = multiply_matrices(&offset_tm, &ctm);
                                let (x, y) = (combined_mat[4], combined_mat[5]);
                                let width = if font_info.is_some() {
                                    ((end_w - start_w) * scale_x).abs()
                                } else {
                                    0.0
                                };
                                items.push(TextItem {
                                    text: expand_ligatures(text),
                                    x,
                                    y,
                                    width,
                                    height: rendered_size,
                                    font: current_font.clone(),
                                    font_size: rendered_size,
                                    page: page_num,
                                    is_bold: is_bold_font(base_font) || desc_bold,
                                    is_italic: is_italic_font(base_font) || desc_italic,
                                    is_underline: false,
                                    is_strikeout: false,
                                    item_type: ItemType::Text,
                                    mcid: None,
                                });
                            }
                        }
                        // Always advance text matrix
                        if font_info.is_some() {
                            text_matrix[4] += total_width_ts * text_matrix[0];
                            text_matrix[5] += total_width_ts * text_matrix[1];
                        }
                    }
                }
            }
            _ => {}
        }
    }

    items
}

/// Get fonts from a Form XObject's Resources
pub(crate) fn get_form_fonts<'a>(
    doc: &'a Document,
    form_dict: &lopdf::Dictionary,
) -> std::collections::BTreeMap<Vec<u8>, &'a lopdf::Dictionary> {
    let mut fonts = std::collections::BTreeMap::new();

    // Get Resources from Form dictionary
    let resources = if let Ok(res_ref) = form_dict.get(b"Resources") {
        if let Ok(obj_ref) = res_ref.as_reference() {
            doc.get_dictionary(obj_ref).ok()
        } else {
            res_ref.as_dict().ok()
        }
    } else {
        return fonts;
    };

    let Some(resources) = resources else {
        return fonts;
    };

    // Get Font dictionary
    let font_dict = if let Ok(font_ref) = resources.get(b"Font") {
        if let Ok(obj_ref) = font_ref.as_reference() {
            doc.get_dictionary(obj_ref).ok()
        } else {
            font_ref.as_dict().ok()
        }
    } else {
        return fonts;
    };

    let Some(font_dict) = font_dict else {
        return fonts;
    };

    // Collect fonts
    for (name, value) in font_dict.iter() {
        if let Ok(obj_ref) = value.as_reference() {
            if let Ok(dict) = doc.get_dictionary(obj_ref) {
                fonts.insert(name.clone(), dict);
            }
        }
    }

    fonts
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{dictionary, Dictionary, Object, Stream};

    fn add_font(doc: &mut Document, base_font: &str) -> ObjectId {
        let widths: Vec<Object> = (0..=255).map(|_| 600.into()).collect();
        doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => Object::Name(base_font.as_bytes().to_vec()),
            "FirstChar" => 0,
            "LastChar" => 255,
            "Widths" => Object::Array(widths),
        })
    }

    fn form_resources(font_id: ObjectId) -> Dictionary {
        dictionary! {
            "Font" => dictionary! {
                "F1" => Object::Reference(font_id),
            },
        }
    }

    fn extract(doc: &Document, form_id: ObjectId) -> Vec<TextItem> {
        extract_form_xobject_text(
            doc,
            form_id,
            3,
            &FontCMaps::default(),
            &[1.0, 0.0, 0.0, 1.0, 100.0, 200.0],
            &mut CMapDecisionCache::new(),
            &mut FontStyleCache::new(),
        )
    }

    #[test]
    fn page_xobjects_support_indirect_resource_dictionaries() {
        let mut doc = Document::with_version("1.7");
        let image_id = doc.add_object(Object::Stream(Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => Object::Name(b"Image".to_vec()),
            },
            vec![0],
        )));
        let form_id = doc.add_object(Object::Stream(Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => Object::Name(b"Form".to_vec()),
            },
            Vec::new(),
        )));
        let ignored_id = doc.add_object(Object::Stream(Stream::new(
            dictionary! { "Subtype" => Object::Name(b"PS".to_vec()) },
            Vec::new(),
        )));
        let xobjects_id = doc.add_object(dictionary! {
            "Im1" => Object::Reference(image_id),
            "Fm1" => Object::Reference(form_id),
            "Ignored" => Object::Reference(ignored_id),
            "Broken" => Object::Reference((999, 0)),
        });
        let resources_id = doc.add_object(dictionary! {
            "XObject" => Object::Reference(xobjects_id),
        });
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Resources" => Object::Reference(resources_id),
        });

        let objects = get_page_xobjects(&doc, page_id);

        assert!(matches!(objects.get("Im1"), Some(XObjectType::Image)));
        assert!(matches!(objects.get("Fm1"), Some(XObjectType::Form(id)) if *id == form_id));
        assert!(!objects.contains_key("Ignored"));
        assert!(!objects.contains_key("Broken"));
        assert!(get_page_xobjects(&doc, (998, 0)).is_empty());
    }

    #[test]
    fn form_fonts_support_direct_and_indirect_resource_dictionaries() {
        let mut doc = Document::new();
        let font_id = add_font(&mut doc, "Helvetica");
        let font_map_id = doc.add_object(dictionary! {
            "F1" => Object::Reference(font_id),
            "Missing" => Object::Reference((999, 0)),
        });
        let resources_id = doc.add_object(dictionary! {
            "Font" => Object::Reference(font_map_id),
        });
        let indirect = dictionary! { "Resources" => Object::Reference(resources_id) };
        let direct = dictionary! { "Resources" => form_resources(font_id) };

        assert_eq!(get_form_fonts(&doc, &indirect).len(), 1);
        assert_eq!(get_form_fonts(&doc, &direct).len(), 1);
        assert!(get_form_fonts(&doc, &Dictionary::new()).is_empty());
    }

    #[test]
    fn form_text_applies_matrix_and_font_style() {
        let mut doc = Document::new();
        let font_id = add_font(&mut doc, "Helvetica-BoldOblique");
        let form_id = doc.add_object(Object::Stream(Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => Object::Name(b"Form".to_vec()),
                "Resources" => form_resources(font_id),
                "Matrix" => vec![2.into(), 0.into(), 0.into(), 2.into(), 10.into(), 20.into()],
            },
            b"BT /F1 10 Tf 1 0 0 1 5 7 Tm (office) Tj ET".to_vec(),
        )));

        let items = extract(&doc, form_id);

        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(item.text, "office");
        assert_eq!(item.page, 3);
        assert!(item.is_bold && item.is_italic);
        assert!((item.font_size - 20.0).abs() < 0.01);
        assert!(item.x > 100.0 && item.y > 200.0);
        assert!(item.width > 0.0);
    }

    #[test]
    fn nested_form_emits_text_and_image_with_local_ctm() {
        let mut doc = Document::new();
        let font_id = add_font(&mut doc, "Helvetica");
        let inner_id = doc.add_object(Object::Stream(Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => Object::Name(b"Form".to_vec()),
                "Resources" => form_resources(font_id),
            },
            b"BT /F1 8 Tf 1 0 0 1 2 3 Tm (nested) Tj ET".to_vec(),
        )));
        let image_id = doc.add_object(Object::Stream(Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => Object::Name(b"Image".to_vec()),
            },
            vec![0],
        )));
        let outer_id = doc.add_object(Object::Stream(Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => Object::Name(b"Form".to_vec()),
                "Resources" => dictionary! {
                    "XObject" => dictionary! {
                        "Inner" => Object::Reference(inner_id),
                        "Photo" => Object::Reference(image_id),
                    },
                },
            },
            b"q 20 0 0 10 4 6 cm /Photo Do Q 1 0 0 1 30 40 cm /Inner Do".to_vec(),
        )));

        let items = extract(&doc, outer_id);

        let image = items
            .iter()
            .find(|item| matches!(&item.item_type, ItemType::Image))
            .unwrap();
        assert_eq!(image.text, "[Image: Photo]");
        assert!((image.width - 20.0).abs() < 0.01);
        assert!((image.height - 10.0).abs() < 0.01);
        assert!(items.iter().any(|item| item.text == "nested"));
    }

    #[test]
    fn recursive_form_reference_stops_at_depth_limit() {
        let mut doc = Document::new();
        let form_id = doc.new_object_id();
        doc.objects.insert(
            form_id,
            Object::Stream(Stream::new(
                dictionary! {
                    "Type" => "XObject",
                    "Subtype" => Object::Name(b"Form".to_vec()),
                    "Resources" => dictionary! {
                        "XObject" => dictionary! {
                            "Loop" => Object::Reference(form_id),
                        },
                    },
                },
                b"/Loop Do".to_vec(),
            )),
        );

        assert!(extract(&doc, form_id).is_empty());
        assert!(extract(&doc, (999, 0)).is_empty());
    }

    #[test]
    fn white_text_is_suppressed_for_supported_color_operators() {
        let mut doc = Document::new();
        let font_id = add_font(&mut doc, "Helvetica");
        let content = b"BT /F1 10 Tf
1 g (gray-hidden) Tj 0 g (gray-shown) Tj
1 1 1 rg (rgb-hidden) Tj 0 0 0 rg (rgb-shown) Tj
0 0 0 0 k (cmyk-hidden) Tj 0 0 0 1 k (cmyk-shown) Tj
1 1 1 scn (sc-hidden) Tj 0 0 0 sc (sc-shown) Tj
0.5 sc (other-shown) Tj ET";
        let form_id = doc.add_object(Object::Stream(Stream::new(
            dictionary! {
                "Resources" => form_resources(font_id),
            },
            content.to_vec(),
        )));

        let text: Vec<_> = extract(&doc, form_id)
            .into_iter()
            .map(|item| item.text)
            .collect();

        assert_eq!(
            text,
            [
                "gray-shown",
                "rgb-shown",
                "cmyk-shown",
                "sc-shown",
                "other-shown",
            ]
        );
    }

    #[test]
    fn tj_array_splits_column_sized_integer_and_real_gaps() {
        let mut doc = Document::new();
        let font_id = add_font(&mut doc, "Helvetica");
        let form_id = doc.add_object(Object::Stream(Stream::new(
            dictionary! { "Resources" => form_resources(font_id) },
            b"BT /F1 10 Tf 1 0 0 1 10 20 Tm [(Left) -1200 (Middle) -1200.5 (Right)] TJ ET".to_vec(),
        )));

        let items = extract(&doc, form_id);

        assert_eq!(
            items
                .iter()
                .map(|item| item.text.as_str())
                .collect::<Vec<_>>(),
            ["Left", "Middle", "Right"]
        );
        assert!(items.windows(2).all(|pair| pair[0].x < pair[1].x));
    }
}
