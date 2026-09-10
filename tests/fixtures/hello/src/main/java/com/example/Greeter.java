package com.example;

/** A second source file, so the build is exercised on more than one. */
public final class Greeter {
    private final String name;

    public Greeter(String name) {
        this.name = name;
    }

    public String greet() {
        return "hello, " + name;
    }
}
