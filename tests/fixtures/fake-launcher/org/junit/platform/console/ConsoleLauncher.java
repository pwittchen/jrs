package org.junit.platform.console;

import java.io.IOException;
import java.io.PrintWriter;
import java.io.StringWriter;
import java.lang.reflect.InvocationTargetException;
import java.lang.reflect.Method;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Comparator;
import java.util.List;
import java.util.Locale;
import java.util.regex.Pattern;
import java.util.stream.Stream;

/**
 * A stand-in for the JUnit Platform console launcher, published into the
 * fixture repository as `junit-platform-console-standalone` so that jrs's test
 * pipeline runs end to end without the network.
 *
 * <p>It speaks the part of the real launcher's language jrs uses: the
 * `execute` subcommand, `--scan-class-path`, `--select-method`,
 * `--select-unique-id`, `--select-class`, `--include-classname`, the tree
 * details in both themes, `--reports-dir` with the legacy XML report, the
 * closing summary block, and `--fail-fast`. Tests are methods annotated with
 * `org.junit.jupiter.api.Test`, run in name order; a test fails by throwing.
 *
 * <p>Its first line of output is every argument it was given, so a test can
 * check what jrs asked for.
 */
public final class ConsoleLauncher {
    private static final String DEFAULT_PATTERN = "^(Test.*|.+[.$]Test.*|.*Tests?)$";

    private record Result(Class<?> type, Method method, Throwable failure, long nanos) {
        String name() {
            return method.getName() + "()";
        }
    }

    public static void main(String[] args) throws Exception {
        System.out.println("fake-launcher " + String.join(" ", args));

        String scan = null;
        String reports = null;
        String include = DEFAULT_PATTERN;
        boolean failFast = false;
        boolean ascii = false;
        boolean testfeed = false;
        List<String> selected = new ArrayList<>();
        List<String> classes = new ArrayList<>();
        for (int i = 0; i < args.length; i++) {
            String arg = args[i];
            switch (arg) {
                case "execute" -> {}
                case "--scan-class-path" -> scan = args[++i];
                case "--select-method" -> selected.add(args[++i]);
                case "--select-unique-id" -> selected.add(fromUniqueId(args[++i]));
                case "--select-class" -> classes.add(args[++i]);
                case "--reports-dir" -> reports = args[++i];
                case "--include-classname" -> include = args[++i];
                case "--include-tag", "--exclude-tag" -> i++;
                case "--fail-fast" -> failFast = true;
                default -> {
                    if (arg.startsWith("--details-theme=")) {
                        ascii = arg.endsWith("ascii");
                    } else if (arg.startsWith("--details=")) {
                        testfeed = arg.equals("--details=testfeed");
                    }
                }
            }
        }

        List<Method> tests = new ArrayList<>();
        if (scan != null) {
            Pattern pattern = Pattern.compile(include);
            for (String name : scanned(Path.of(scan))) {
                if (pattern.matcher(name).matches()) {
                    tests.addAll(testsOf(Class.forName(name)));
                }
            }
        }
        for (String name : classes) {
            tests.addAll(testsOf(Class.forName(name)));
        }
        for (String selector : selected) {
            String[] parts = selector.split("#", 2);
            String method = parts[1].replaceAll("\\(.*\\)$", "");
            tests.add(Class.forName(parts[0]).getDeclaredMethod(method));
        }

        String branch = ascii ? "+-- " : "├─ ";
        String ok = ascii ? "[OK]" : "✔";
        String failed = ascii ? "[X]" : "✘";
        List<Result> results = new ArrayList<>();
        boolean cancelled = false;
        for (Method method : tests) {
            // The test feed names each test as it starts and as it ends, the
            // way the real one does; the tree marks it once, when it ends.
            String feed = "JUnit Jupiter > " + method.getDeclaringClass().getSimpleName() + " > "
                    + method.getName() + "()";
            if (testfeed) {
                System.out.println(feed + " :: STARTED");
                System.out.flush();
            }
            long start = System.nanoTime();
            Throwable failure = null;
            try {
                method.setAccessible(true);
                var constructor = method.getDeclaringClass().getDeclaredConstructor();
                constructor.setAccessible(true);
                method.invoke(constructor.newInstance());
            } catch (InvocationTargetException e) {
                failure = e.getCause();
            }
            Result result = new Result(method.getDeclaringClass(), method, failure, System.nanoTime() - start);
            results.add(result);
            if (testfeed) {
                System.out.println(feed + (failure == null ? " :: SUCCESSFUL" : " :: FAILED"));
                if (failure != null) {
                    System.out.println("\t" + failure);
                    for (StackTraceElement frame : failure.getStackTrace()) {
                        System.out.println("\t\tat " + frame);
                    }
                }
            } else {
                System.out.println(branch + result.name() + " "
                        + (failure == null ? ok : failed + " " + failure.getMessage()));
            }
            System.out.flush();
            if (failure != null && failFast) {
                cancelled = true;
                break;
            }
        }

        long bad = results.stream().filter(r -> r.failure != null).count();
        if (reports != null) {
            writeReport(Path.of(reports), results);
        }
        System.out.println();
        System.out.println(String.format("[%10d tests found           ]", tests.size()));
        System.out.println(String.format("[%10d tests successful      ]", results.size() - bad));
        System.out.println(String.format("[%10d tests failed          ]", bad));
        if (cancelled) {
            System.out.println();
            System.out.println("Test execution was cancelled due to --fail-fast mode.");
        }
        System.exit(bad == 0 ? 0 : 1);
    }

    /** `[engine:junit-jupiter]/[class:a.B]/[method:c()]` as `a.B#c()`. */
    private static String fromUniqueId(String id) {
        String type = id.replaceAll(".*\\[class:([^\\]]+)\\].*", "$1");
        String method = id.replaceAll(".*\\[method:([^\\]]+)\\].*", "$1");
        return type + "#" + method;
    }

    private static List<String> scanned(Path root) throws IOException {
        try (Stream<Path> files = Files.walk(root)) {
            return files.filter(p -> p.toString().endsWith(".class") && !p.toString().contains("$"))
                    .map(p -> root.relativize(p).toString().replace(root.getFileSystem().getSeparator(), "."))
                    .map(n -> n.substring(0, n.length() - ".class".length()))
                    .sorted()
                    .toList();
        }
    }

    private static List<Method> testsOf(Class<?> type) {
        return Arrays.stream(type.getDeclaredMethods())
                .filter(m -> m.isAnnotationPresent(org.junit.jupiter.api.Test.class))
                .sorted(Comparator.comparing(Method::getName))
                .toList();
    }

    /** The legacy XML report, in the shape the real launcher writes it. */
    private static void writeReport(Path dir, List<Result> results) throws IOException {
        Files.createDirectories(dir);
        StringBuilder xml = new StringBuilder("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
        long bad = results.stream().filter(r -> r.failure != null).count();
        xml.append("<testsuite name=\"JUnit Jupiter\" tests=\"").append(results.size())
                .append("\" skipped=\"0\" failures=\"").append(bad).append("\" errors=\"0\">\n");
        for (Result r : results) {
            xml.append("<testcase name=\"").append(attr(r.name())).append("\" classname=\"")
                    .append(attr(r.type.getName())).append("\" time=\"")
                    .append(String.format(Locale.ROOT, "%.3f", r.nanos / 1e9)).append("\">\n");
            if (r.failure != null) {
                StringWriter trace = new StringWriter();
                r.failure.printStackTrace(new PrintWriter(trace));
                xml.append("<failure message=\"").append(attr(String.valueOf(r.failure.getMessage())))
                        .append("\" type=\"").append(attr(r.failure.getClass().getName())).append("\">")
                        .append(cdata(trace.toString())).append("</failure>\n");
            }
            xml.append("<system-out>")
                    .append(cdata("\nunique-id: [engine:junit-jupiter]/[class:" + r.type.getName()
                            + "]/[method:" + r.name() + "]\ndisplay-name: " + r.name() + "\n"))
                    .append("</system-out>\n</testcase>\n");
        }
        xml.append("</testsuite>\n");
        Files.writeString(dir.resolve("TEST-junit-jupiter.xml"), xml, StandardCharsets.UTF_8);
    }

    private static String attr(String s) {
        return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;").replace("\"", "&quot;");
    }

    private static String cdata(String s) {
        return "<![CDATA[" + s.replace("]]>", "]]]]><![CDATA[>") + "]]>";
    }
}
