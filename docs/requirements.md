# Overview

This app is intended to accelerate development when using a GenAI assistance such a claude code by helping add an automated structure to your new software development process. When you start the claudmux terminal app in a project folder it will look for a "tasks" folder in either the top level directory of the project or under a  'docs' folder. It will load the list of Stories and Tasks in the left pane of its terminal UI. Grouping Tasks by story based on the "name" of the story. When you select a task in the left nav, the right pane will show you four tabs. The first tab is the details of the task as scrapped from its markdown file. At the bottom of that pane is a text box which allows you to enter an optional supplemental prompt for starting the task in claude code. Below the prompt box is a list of questions with answer boxes. This is where questions from your team of agents will appear for you to answer. All questions and answers get recorded in the task file for posterity. The second tab is an instance of claude code which claudmux is managing for executing your task. The third tab is the status of your claudmux team which is working on your task. These agents work sequentially to accomplish your task with high quality but they may move the task back to a previous agent in the workflow if a later agent (e.g. code quality agent) detects a problem. We'll cover this in more detail in the "Work Flow" section below. The fourth tab is a code review tab that shows you a code diff of what the agents have produced thus far for the task. You'll be able to add comments to the code review and send the task back to the agent workflow for further "revision".

# Work Flow

The persona of each agent is configured via the agent's personality file under the .claudmux folder in the project directory. This folder is created automatically by claudmux the first time it runs in your project and it also creates the agents sub-folder within this directory with a text file per agent pre-populated with the default personality clawdmux ships with for each agent. Any of the agents may prompt the team leader (human) for additional information by asking questions.

```
1. Intake Agent - This agent is responsible for reviewing the task file and suggesting any missing fields as well as prompting the team leader (human) for missing details it can not infer.
2. Design Agent - This agent is responsible for reviewing the task as well as the existing state of the project to propose any relevant design implications required to complete this task. 
3. Planning Agent - This agent is responsible for reviewing the task and design proposal of the Design Agent in order to create an implementation plan that is added to the task file for approval by the team leader.
4. Implementation Agent - This agent's job is to implement the plan.
5. Code Quality Agent - This agent's job is to ensure the code has adequate test coverage, builds without errors, and follows the project's coding standards (e.g. no clippy error in the case of rust.) This agent may kick the task back to the implementation agent if it finds non-trivial issues that it can not address itself.
6. Security Review Agent - This agent reviews the code produced thus far for any security concerns. It may kick the task back to a previous agent to address its findings.
7. Code Review Agent - This agent has two jobs: (1) independently reviews the code for bugs, maintainability concerns, and adherence to project standards, kicking back to the appropriate earlier agent (Implementation, Design, or Planning) if issues are found; (2) once its own review passes, ensures that any human reviewer feedback is also addressed via kickbacks. If no issues remain and the human approves, it prepares a commit message for the work.

# Sample Task File

Story: 1. Big Story
Task: 1.1 First Task
Status: IN_PROGRESS
Assigned To: [Planning Agent]

## Description

<description of the task>

## Starting Prompt

<optional starting prompt provided by team leader>

## Questions

Q1 [Intake Agent]: What language do you want to use for this task?
A1: Lets use rust, it is well suited to this.

## Design

<Design considerations to use for this task>

## Implementation Plan

<Plan to use for this task>

## Work Log

1 2026-02-10T10:00:01 [Design Agent] updated task with design and assigned task to [Planning Agent] for next step.
```