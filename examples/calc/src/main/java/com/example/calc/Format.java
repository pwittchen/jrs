package com.example.calc;

import java.text.DecimalFormat;
import java.text.DecimalFormatSymbols;
import java.util.Locale;

/**
 * Number formatting, in Java. Scala's {@code Main} calls it, and it calls
 * Scala's {@link Calculator} back.
 */
public final class Format {
    private Format() {}

    /** {@code 2.5} as {@code 2.5}, {@code 3.0} as {@code 3}. */
    public static String number(double value) {
        return new DecimalFormat("0.####", DecimalFormatSymbols.getInstance(Locale.ROOT)).format(value);
    }

    /** Evaluate an expression and format the result, or say what is wrong with it. */
    public static String evaluate(String expression) {
        try {
            return number(Calculator.evaluate(expression));
        } catch (IllegalArgumentException | ArithmeticException e) {
            return "error: " + e.getMessage();
        }
    }
}
