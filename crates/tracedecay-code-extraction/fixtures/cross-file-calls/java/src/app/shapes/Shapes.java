package app.shapes;

import static app.util.Util.clamp;

import app.math.*;

public final class Shapes {
    public static int area(int width, int height) {
        return clamp(width, 0, 100) * height;
    }

    public static int perimeter(int width, int height) {
        return MathOps.total(new int[] {clamp(width, 0, 100), height}) * 2;
    }
}
