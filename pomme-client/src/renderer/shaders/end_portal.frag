#version 450

// Vanilla `rendertype_end_portal.fsh`: the end sky, then up to sixteen
// layers of the portal's stars, each scrolled, turned and scaled by its own
// amount, all projected from the screen position.

layout(set = 1, binding = 0) uniform texture2D end_sky;
layout(set = 1, binding = 1) uniform sampler end_sky_sampler;
layout(set = 2, binding = 0) uniform texture2D end_portal;
layout(set = 2, binding = 1) uniform sampler end_portal_sampler;

layout(push_constant) uniform Push {
    // Vanilla `GameTime`: the game clock's day fraction.
    float game_time;
};

layout(location = 0) in vec4 tex_proj;
layout(location = 1) flat in int portal_layers;

layout(location = 0) out vec4 out_color;

const vec3[] COLORS = vec3[](
    vec3(0.022087, 0.098399, 0.110818),
    vec3(0.011892, 0.095924, 0.089485),
    vec3(0.027636, 0.101689, 0.100326),
    vec3(0.046564, 0.109883, 0.114838),
    vec3(0.064901, 0.117696, 0.097189),
    vec3(0.063761, 0.086895, 0.123646),
    vec3(0.084817, 0.111994, 0.166380),
    vec3(0.097489, 0.154120, 0.091064),
    vec3(0.106152, 0.131144, 0.195191),
    vec3(0.097721, 0.110188, 0.187229),
    vec3(0.133516, 0.138278, 0.148582),
    vec3(0.070006, 0.243332, 0.235792),
    vec3(0.196766, 0.142899, 0.214696),
    vec3(0.047281, 0.315338, 0.321970),
    vec3(0.204675, 0.390010, 0.302066),
    vec3(0.080955, 0.314821, 0.661491)
);

const mat4 SCALE_TRANSLATE = mat4(
    0.5, 0.0, 0.0, 0.25,
    0.0, 0.5, 0.0, 0.25,
    0.0, 0.0, 1.0, 0.0,
    0.0, 0.0, 0.0, 1.0
);

// Vanilla `mat2_rotate_z` (matrix.glsl).
mat2 mat2_rotate_z(float radians) {
    return mat2(
        cos(radians), -sin(radians),
        sin(radians), cos(radians)
    );
}

mat4 end_portal_layer(float layer) {
    mat4 translate = mat4(
        1.0, 0.0, 0.0, 17.0 / layer,
        0.0, 1.0, 0.0, (2.0 + layer / 1.5) * (game_time * 1.5),
        0.0, 0.0, 1.0, 0.0,
        0.0, 0.0, 0.0, 1.0
    );
    mat2 rotate = mat2_rotate_z(radians((layer * layer * 4321.0 + layer * 9.0) * 2.0));
    mat2 scale = mat2((4.5 - layer / 4.0) * 2.0);
    return mat4(scale * rotate) * translate * SCALE_TRANSLATE;
}

void main() {
    vec3 color = textureProj(sampler2D(end_sky, end_sky_sampler), tex_proj).rgb * COLORS[0];
    for (int i = 0; i < portal_layers; i++) {
        color += textureProj(sampler2D(end_portal, end_portal_sampler), tex_proj * end_portal_layer(float(i + 1))).rgb * COLORS[i];
    }
    out_color = vec4(color, 1.0);
}
