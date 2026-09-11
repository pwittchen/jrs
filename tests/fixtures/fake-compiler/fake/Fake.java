package fake;

import java.io.File;
import java.io.IOException;
import java.io.OutputStream;
import java.net.URI;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.Set;
import javax.tools.FileObject;
import javax.tools.ForwardingJavaFileManager;
import javax.tools.JavaCompiler;
import javax.tools.JavaFileManager;
import javax.tools.JavaFileObject;
import javax.tools.SimpleJavaFileObject;
import javax.tools.StandardJavaFileManager;
import javax.tools.ToolProvider;

/**
 * A stand-in for kotlinc, scalac and groovyc, so jrs's tests can drive the
 * whole multi-language pipeline without 60 MB of real compilers.
 *
 * <p>It reads the arguments the real compiler would get, writes them down
 * beside the output directory for the test to read, and compiles the
 * "foreign" sources — which hold valid Java — with the JDK's own compiler.
 * Like kotlinc and scalac it only reads the Java sources, for their symbols;
 * like groovyc in joint mode, it compiles them too when asked to.
 */
public final class Fake {
    private Fake() {}

    public static int compile(String tool, String[] args, String extension, boolean compilesJava)
            throws IOException {
        String out = null;
        String classpath = "";
        List<Path> sources = new ArrayList<>();
        for (int i = 0; i < args.length; i++) {
            String arg = args[i];
            if (arg.equals("-d")) {
                out = args[++i];
            } else if (arg.equals("-cp") || arg.equals("-classpath")) {
                classpath = args[++i];
            } else if (arg.endsWith(extension) || arg.endsWith(".java")) {
                sources.add(Path.of(arg));
            }
        }
        if (out == null) {
            System.err.println(tool + ": no -d");
            return 2;
        }
        Path output = Path.of(out);

        List<String> record = new ArrayList<>();
        record.add(tool);
        record.addAll(Arrays.asList(args));
        record.add("groovy.target.bytecode=" + System.getProperty("groovy.target.bytecode"));
        // The compiler's one transitive dependency: on its classpath only if
        // jrs resolved the compiler's graph and not just its jar.
        record.add("support=" + fake.support.Support.name());
        Files.write(output.resolveSibling(output.getFileName() + ".fake-" + tool), record);

        JavaCompiler javac = ToolProvider.getSystemJavaCompiler();
        StandardJavaFileManager files =
                javac.getStandardFileManager(null, null, StandardCharsets.UTF_8);
        JavaFileManager manager = compilesJava ? files : new ForeignOnly(files);
        List<JavaFileObject> units = new ArrayList<>();
        for (Path source : sources) {
            units.add(new Source(source));
        }
        List<String> options = new ArrayList<>(List.of("-d", out, "-proc:none"));
        if (!classpath.isEmpty()) {
            options.add("-cp");
            options.add(classpath);
        }
        boolean ok = javac.getTask(null, manager, null, options, null, units).call();
        return ok ? 0 : 1;
    }

    /** Options of the doc tools whose value is the next argument. */
    private static final Set<String> DOC_VALUE_OPTIONS =
            Set.of("-classpath", "-project", "-project-version", "-doc-title", "-doc-version",
                    "-encoding");

    /**
     * A stand-in for scaladoc and groovydoc. Like the real ones it wants its
     * output directory to exist already, and like Groovydoc it looks each
     * relative source up under its {@code -sourcepath}; an input it cannot
     * find fails the run. It writes an index page listing its arguments, one
     * per line, then the classpath it ran with, for the test to read.
     */
    public static int document(String tool, String[] args) throws IOException {
        String out = null;
        List<String> roots = new ArrayList<>();
        List<String> inputs = new ArrayList<>();
        for (int i = 0; i < args.length; i++) {
            String arg = args[i];
            if (arg.equals("-d")) {
                out = args[++i];
            } else if (DOC_VALUE_OPTIONS.contains(arg)) {
                i++;
            } else if (arg.startsWith("-sourcepath=")) {
                roots = Arrays.asList(arg.substring("-sourcepath=".length()).split(File.pathSeparator));
            } else if (!arg.startsWith("-")) {
                inputs.add(arg);
            }
        }
        if (out == null || !Files.isDirectory(Path.of(out))) {
            System.err.println(tool + ": " + out + " does not exist or is not a directory");
            return 2;
        }
        for (String input : inputs) {
            boolean found = roots.isEmpty()
                    ? Files.exists(Path.of(input))
                    : roots.stream().anyMatch(root -> Files.exists(Path.of(root, input)));
            if (!found) {
                System.err.println(tool + ": no such input: " + input);
                return 1;
            }
        }
        List<String> page = new ArrayList<>();
        page.add(tool);
        page.addAll(Arrays.asList(args));
        page.add("classpath=" + System.getProperty("java.class.path"));
        page.add("support=" + fake.support.Support.name());
        Files.write(Path.of(out, "index.html"), page);
        return 0;
    }

    /** A source file of any extension, read as Java. */
    private static final class Source extends SimpleJavaFileObject {
        private final Path path;

        Source(Path path) {
            super(path.toUri(), Kind.SOURCE);
            this.path = path;
        }

        @Override
        public CharSequence getCharContent(boolean ignoreEncodingErrors) throws IOException {
            return Files.readString(path);
        }

        @Override
        public boolean isNameCompatible(String simpleName, Kind kind) {
            String name = path.getFileName().toString();
            return name.substring(0, name.lastIndexOf('.')).equals(simpleName);
        }
    }

    /** Throws away the classes of `.java` sources, as kotlinc and scalac do. */
    private static final class ForeignOnly extends ForwardingJavaFileManager<JavaFileManager> {
        ForeignOnly(JavaFileManager delegate) {
            super(delegate);
        }

        @Override
        public JavaFileObject getJavaFileForOutput(
                Location location, String className, JavaFileObject.Kind kind, FileObject sibling)
                throws IOException {
            if (sibling != null && sibling.getName().endsWith(".java")) {
                return new SimpleJavaFileObject(URI.create("discard:///" + className), kind) {
                    @Override
                    public OutputStream openOutputStream() {
                        return OutputStream.nullOutputStream();
                    }
                };
            }
            return super.getJavaFileForOutput(location, className, kind, sibling);
        }
    }
}
