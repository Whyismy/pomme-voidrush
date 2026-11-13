#version 450

// The enchantment glint (vanilla `glint.vsh`): a foil item's mesh again,
// at the very same depth, with the glint texture slid across its own UVs.

#include "item_instance.glsl"
#include "fog.glsl"

#include "camera_ubo.glsl"

layout(location = 0) in vec3 position;
layout(location = 1) in vec2 tex_coords;
layout(location = 3) in vec4 normal_packed;

layout(location = 0) out vec2 v_tex_coords;
layout(location = 1) out float v_fade;

// The glint passes the depth test only where it equals the item's own, so
// the position must come out bit for bit as the item shaders' does.
invariant gl_Position;

// `TextureTransform.setupGlintTexturing`'s turn: ten degrees.
const float TURN_COS = 0.98480775;
const float TURN_SIN = 0.17364818;

void main() {
    vec4 world_pos = model * vec4(position, 1.0);
    vec3 rel = world_pos.xyz - camera_pos.xyz;
    gl_Position = view_proj * vec4(rel, 1.0);

    // Scaled, turned, then scrolled (the texture matrix, right to left).
    vec2 uv = tex_coords * glint.zw;
    // A compass or a clock, whose sprite changes under it (vanilla's
    // `SheetedDecalTextureGenerator`): the glint lies on each face by where
    // the face is in the model, a 128th of the texture a block, at the size
    // its view shows it (`fog_env.z`: half in an icon, three quarters in
    // the hand).
    if ((outline_color & 0x40000000u) != 0u) {
        vec3 at = position / fog_env.z;
        vec3 facing = abs(normal_packed.xyz);
        vec2 on_face = facing.z >= facing.x && facing.z >= facing.y
            ? at.xy
            : (facing.x >= facing.y ? at.zy : at.xz);
        uv = -on_face * (8.0 / 128.0);
    }
    v_tex_coords = vec2(TURN_COS * uv.x - TURN_SIN * uv.y, TURN_SIN * uv.x + TURN_COS * uv.y)
        + vec2(-glint.x, glint.y);
    v_fade = 1.0 - clamp(total_fog_value(rel, fog_env, camera_pos.w, fog_color.w), 0.0, 1.0);
}
