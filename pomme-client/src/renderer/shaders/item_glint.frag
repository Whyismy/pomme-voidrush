#version 450

// Vanilla `glint.fsh`: the glint texture, faded by fog and by the Glint
// Strength option, added to what is drawn (the pipeline's blend).

layout(set = 1, binding = 0) uniform texture2D glint_texture;
layout(set = 1, binding = 1) uniform sampler glint_sampler;

layout(location = 0) in vec2 v_tex_coords;
layout(location = 1) in float v_fade;

layout(location = 0) out vec4 out_color;

// Vanilla's default Glint Strength.
// TODO: the Glint Strength and Glint Speed accessibility options.
const float GLINT_ALPHA = 0.75;

void main() {
    vec4 color = texture(sampler2D(glint_texture, glint_sampler), v_tex_coords);
    if (color.a < 0.1) discard;
    out_color = vec4(color.rgb * v_fade * GLINT_ALPHA, color.a);
}
