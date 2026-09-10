package fake.support;

/** Published as the fake compilers' one transitive dependency. */
public final class Support {
    private Support() {}

    public static String name() {
        return "fake-compiler-support";
    }
}
