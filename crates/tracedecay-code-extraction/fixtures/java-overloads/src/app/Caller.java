package app;

public class Caller {
    void run() {
        Validate.notNull(this);
        Validate.notNull(this, "caller");
        Validate.check(1);
        Validate.format("x", "y");
        Validate.format("x");
    }
}
