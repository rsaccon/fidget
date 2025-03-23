let scale = 3;

let x = x * scale;
let y = y * scale;
let z = z * scale;

let sphere = sqrt(square(x) + square(y) + square(z)) - 1;
let sphere = move(sphere, 0.5, 0.0);
draw(sphere);
