// The enchantment glint over an entity's model (vanilla `glint.vsh`): the
// mesh again, at the very same depth, with a glint texture slid across its
// UVs at GLINT_SCALE, which the including shader defines.

#include "fog.glsl"

#include "camera_ubo.glsl"

layout(location = 0) in vec3 position;
layout(location = 1) in vec2 tex_coords;

// The instance's model matrix, as entity.vert reads it.
layout(location = 3) in vec4 i_model_0;
layout(location = 4) in vec4 i_model_1;
layout(location = 5) in vec4 i_model_2;
layout(location = 6) in vec4 i_model_3;

layout(location = 0) out vec2 v_tex_coords;
layout(location = 1) out float v_fade;

// The glint passes the depth test only where it equals the armor's own, so
// the position must come out bit for bit as entity.vert's does.
invariant gl_Position;

// `TextureTransform.setupGlintTexturing`'s turn, ten degrees.
const float TURN_COS = 0.98480775;
const float TURN_SIN = 0.17364818;

void main() {
    mat4 model = mat4(i_model_0, i_model_1, i_model_2, i_model_3);
    vec4 world_pos = model * vec4(position, 1.0);
    vec3 rel = world_pos.xyz - camera_pos.xyz;
    gl_Position = view_proj * vec4(rel, 1.0);

    vec2 uv = tex_coords * GLINT_SCALE;
    v_tex_coords = vec2(TURN_COS * uv.x - TURN_SIN * uv.y, TURN_SIN * uv.x + TURN_COS * uv.y)
        + vec2(-glint.x, glint.y);
    v_fade = 1.0 - clamp(total_fog_value(rel, fog_env, camera_pos.w, fog_color.w), 0.0, 1.0);
}
