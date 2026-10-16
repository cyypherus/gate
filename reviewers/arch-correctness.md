# Correctness

## 1. Placement

For every problem solved in this code: can it be solved elsewhere? If so, which place is correct?

If the place chosen was not correct, reject the PR.

## 2. Vacuous or Redundant Tests

New tests must not duplicate guarantees which are already upheld by existing tests. Assertions made by tests must be valuable, and likely to catch a real bug, otherwise the test is a waste of time.

## 3. Feature logic in Views

Reject logic which is in views & is not strictly and directly related to the rendering of a feature, as opposed to logic which is exists for the feature itself & is rendering agnostic.

## 4. Code hardcoded to the present

If code can trivially be written to be extensible to support new cases which serve similar purposes, encourage the code to be written generally. It's always a balance, don't try to predict the distant future, but encourage code which is aware of extensions that have been made in the past.
