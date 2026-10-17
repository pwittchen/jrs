import java.lang.annotation.ElementType;
import java.lang.annotation.Retention;
import java.lang.annotation.RetentionPolicy;
import java.lang.annotation.Target;
import java.util.ArrayList;
import java.util.Collections;
import java.util.Comparator;
import java.util.EnumMap;
import java.util.HashMap;
import java.util.Iterator;
import java.util.List;
import java.util.Map;
import java.util.Objects;
import java.util.Optional;
import java.util.function.BiFunction;
import java.util.function.Function;
import java.util.function.Supplier;
import java.util.stream.Collectors;
import java.util.stream.IntStream;

/**
 * The workload jrs trains javac's AOT cache on (specs/todo/BEATING_THE_DAEMON_3.md
 * §2), compiled into a scratch directory and thrown away. It is never run: it only
 * has to make javac parse, attribute, check and write most of what a project's
 * sources hold — generics and their inference, lambdas, method references,
 * records, enums, nested and anonymous classes, annotations, switch expressions,
 * try-with-resources and string constants.
 */
public final class Training {
    private Training() {}

    @Retention(RetentionPolicy.RUNTIME)
    @Target({ElementType.TYPE, ElementType.METHOD, ElementType.FIELD})
    @interface Note {
        String value() default "";

        int weight() default 1;
    }

    enum Shape {
        CIRCLE(1.0),
        SQUARE(2.0) {
            @Override
            double area(double side) {
                return side * side;
            }
        },
        TRIANGLE(3.0);

        private final double factor;

        Shape(double factor) {
            this.factor = factor;
        }

        double area(double size) {
            return Math.PI * size * size * factor;
        }
    }

    sealed interface Expr permits Num, Add, Mul {}

    record Num(long value) implements Expr {}

    record Add(Expr left, Expr right) implements Expr {}

    record Mul(Expr left, Expr right) implements Expr {}

    record Pair<A, B>(A first, B second) {
        Pair {
            Objects.requireNonNull(first);
        }

        <C> Pair<A, C> withSecond(C value) {
            return new Pair<>(first, value);
        }
    }

    interface Visitor<R> {
        R visit(Node node);

        default <S> Visitor<S> andThen(Function<? super R, ? extends S> after) {
            return node -> after.apply(visit(node));
        }
    }

    abstract static class Node implements Comparable<Node> {
        @Note("name")
        protected final String name;

        Node(String name) {
            this.name = name;
        }

        abstract int size();

        @Override
        public int compareTo(Node other) {
            return Comparator.comparing((Node n) -> n.name)
                    .thenComparingInt(Node::size)
                    .compare(this, other);
        }
    }

    static final class Leaf extends Node {
        Leaf(String name) {
            super(name);
        }

        @Override
        int size() {
            return 1;
        }
    }

    static final class Branch extends Node implements Iterable<Node> {
        private final List<Node> children = new ArrayList<>();

        Branch(String name, Node... children) {
            super(name);
            Collections.addAll(this.children, children);
        }

        @Override
        int size() {
            return 1 + children.stream().mapToInt(Node::size).sum();
        }

        @Override
        public Iterator<Node> iterator() {
            return children.iterator();
        }
    }

    static long eval(Expr expr) {
        if (expr instanceof Num n) {
            return n.value();
        } else if (expr instanceof Add a) {
            return eval(a.left()) + eval(a.right());
        } else if (expr instanceof Mul m) {
            return eval(m.left()) * eval(m.right());
        }
        throw new IllegalStateException("unknown: " + expr);
    }

    static String describe(Shape shape, int sides) {
        String kind = switch (shape) {
            case CIRCLE -> "round";
            case SQUARE -> {
                String s = sides == 4 ? "square" : "odd";
                yield s;
            }
            default -> "pointed";
        };
        return String.format("%s:%d:%s", shape.name().toLowerCase(), sides, kind);
    }

    @Note(value = "generic", weight = 2)
    static <T extends Comparable<? super T>> Optional<T> largest(Iterable<? extends T> items) {
        T best = null;
        for (T item : items) {
            if (best == null || item.compareTo(best) > 0) {
                best = item;
            }
        }
        return Optional.ofNullable(best);
    }

    static <K, V> Map<V, List<K>> invert(Map<K, V> map) {
        return map.entrySet().stream()
                .collect(Collectors.groupingBy(
                        Map.Entry::getValue,
                        HashMap::new,
                        Collectors.mapping(Map.Entry::getKey, Collectors.toList())));
    }

    static int count(Node node) {
        Visitor<Integer> sizes = Node::size;
        Visitor<String> labels = sizes.andThen(n -> "n" + n);
        int total = labels.visit(node).length();
        if (node instanceof Branch branch) {
            for (Node child : branch) {
                total += count(child);
            }
        }
        return total;
    }

    static String read(Supplier<? extends AutoCloseable> opener) {
        StringBuilder out = new StringBuilder();
        try (AutoCloseable resource = opener.get()) {
            out.append(resource);
        } catch (RuntimeException e) {
            out.append("runtime: ").append(e.getMessage());
        } catch (Exception e) {
            out.append("checked: ").append(e.getMessage());
        } finally {
            out.append('.');
        }
        return out.toString();
    }

    static Object everything() {
        Map<Shape, Double> areas = new EnumMap<>(Shape.class);
        for (Shape shape : Shape.values()) {
            areas.put(shape, shape.area(2));
        }
        BiFunction<Integer, Integer, Integer> add = Integer::sum;
        List<Pair<String, Integer>> pairs = IntStream.rangeClosed(1, 10)
                .mapToObj(i -> new Pair<>("k" + i, add.apply(i, i)))
                .filter(p -> p.second() % 3 != 0)
                .sorted(Comparator.comparing(Pair<String, Integer>::second).reversed())
                .collect(Collectors.toList());
        Node tree = new Branch("root", new Leaf("a"), new Branch("b", new Leaf("c")));
        Runnable task = new Runnable() {
            private int runs;

            @Override
            public void run() {
                runs++;
            }
        };
        task.run();
        long value = eval(new Add(new Num(2), new Mul(new Num(3), new Num(4))));
        Map<Integer, List<String>> inverted = invert(Map.of("x", 1, "y", 2, "z", 1));
        return List.of(
                areas,
                pairs.get(0).withSecond(describe(Shape.SQUARE, 4)),
                largest(List.of("pear", "apple", "quince")).orElse(""),
                count(tree),
                value,
                inverted,
                read(() -> () -> {}),
                """
                a text block, \
                for the constant pool""");
    }
}
