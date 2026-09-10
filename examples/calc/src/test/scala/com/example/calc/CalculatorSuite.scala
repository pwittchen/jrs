package com.example.calc

// Named *Suite: jrs widens the launcher's class-name pattern for non-Java tests.
class CalculatorSuite extends munit.FunSuite:
  test("multiplication binds tighter than addition") {
    assertEquals(Calculator.evaluate("1 + 2 * 3"), 7.0)
  }

  test("parentheses group") {
    assertEquals(Calculator.evaluate("(1 + 2) * 3"), 9.0)
  }

  test("a leading minus negates") {
    assertEquals(Calculator.evaluate("-(2.5 - 4) / 0.5"), 3.0)
  }

  test("division by zero is an error") {
    intercept[ArithmeticException](Calculator.evaluate("1 / 0"))
  }

  test("a dangling operator is an error") {
    val e = intercept[IllegalArgumentException](Calculator.evaluate("1 +"))
    assert(e.getMessage.contains("unexpected end"), e.getMessage)
  }

  test("the Java formatter reads the Scala result") {
    assertEquals(Format.evaluate("10 / 4"), "2.5")
    assertEquals(Format.evaluate("1 / 0"), "error: division by zero")
  }
